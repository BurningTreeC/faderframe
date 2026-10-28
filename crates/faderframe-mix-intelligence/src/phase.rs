//! Phase and timing between two tracks heard together: measured on an
//! excerpt of both (rendered as analysed: processed — each as it reaches
//! the master, so their sum is what the mix holds of the two — or before
//! processing).
//!
//! **Which pairs** ([`candidates`]). Never every pair: a pair is compared
//! when the user selected both (scope "selected tracks"), when their roles
//! say they share a source (parts of one drum kit against the kit's
//! loudest "Drums" track — the overheads, usually —, or tracks with the
//! same role against the group's loudest: kick in and out, snare top and
//! bottom, a bass's DI and microphone, doubled parts), a pair carrying the
//! same audio ([`crate::same`]), a kick and a bass, a pair a masking
//! finding names, or a pair whose band levels rise and fall together over
//! time ([`covariation`], a screening of every pair on the compact
//! profiles: `O(N² · frames · 7)`). At most [`PhaseRule::max_pairs`] in
//! that order are measured, each over an excerpt of
//! [`PhaseRule::excerpt_seconds`] where both play (the busiest for the
//! pair, the same-audio or masking events preferred, ties going to where most
//! compared tracks play so excerpts are shared): the cost is bounded by
//! `max_pairs × excerpt` whatever the session's size.
//!
//! **What is measured** ([`measure_pair`]), on frames (Hann, the analysis'
//! FFT size, half overlap) where both are active:
//!
//! * the *delay and polarity* by Auto-Align's estimator
//!   ([`faderframe_analysis::align`]: maximum-likelihood weighted GCC,
//!   sub-sample lag, its sign, a robust z-score's confidence and the
//!   peak's clarity) on the two mono fold-downs;
//! * per band (the analysis' third octaves), from the averaged
//!   cross-spectrum: the debiased magnitude-squared *coherence*, the
//!   cross-spectrum's *phase* now and its *residual* once the measured
//!   delay and polarity are taken out (zero for a pure delay or
//!   inversion; what is left is frequency-dependent: filters, different
//!   microphones, all-pass shifts);
//! * per band the *sum against the energies*: `P(a+b) / (P(a) + P(b))` —
//!   0 dB when the two are unrelated (on average they neither add nor
//!   cancel), up to +3 dB for equal signals in phase, falling without
//!   bound as they cancel — per channel as they reach their destination,
//!   in a mono fold-down, as it would be with the measured delay and
//!   polarity corrected, and with only the polarity flipped;
//! * *onsets* of both mono fold-downs (`faderframe_audio_files::onsets`),
//!   paired one to one within [`PairSettings::match_ms`]: their offsets'
//!   median and spread (median absolute deviation).

use crate::bands::BandLayout;
use crate::config::{AnalysisConfig, ConfigError, PhaseRule};
use crate::finding::{Finding, Subject, TimeSpan};
use crate::profile::{TrackProfile, power_db};
use crate::rules::TrackInput;
use faderframe_analysis::align::{AlignSettings, analyse as align};
use faderframe_core::{TrackId, TrackRole};
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

/// Frames compared at most (evenly spread over the both-active ones).
const MAX_FRAMES: usize = 400;
/// Frames quieter than this under the track's loudest (as a power ratio,
/// 50 dB) are not active; under `SILENCE` (mean square) neither.
const ACTIVE: f64 = 1e-5;
const SILENCE: f64 = 1e-10;

#[derive(Debug, Error, PartialEq)]
pub enum PhaseError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("no audio channels to compare")]
    NoChannels,
    #[error("the two signals differ in length")]
    Ragged,
    #[error("the comparison was cancelled")]
    Cancelled,
}

/// Why a pair was compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PairReason {
    /// Both selected (analysing the selected tracks).
    Selected,
    /// Their roles say they share a source (one drum kit, the same role).
    SameSource,
    /// They carry the same audio ([`crate::same`]).
    SameAudio,
    KickAndBass,
    /// A masking finding names them.
    Masking,
    /// Their band levels rise and fall together.
    CoVaries,
}

impl PairReason {
    pub fn label(self) -> &'static str {
        match self {
            PairReason::Selected => "both selected",
            PairReason::SameSource => "their roles say they share a source",
            PairReason::SameAudio => "they carry the same audio",
            PairReason::KickAndBass => "kick and bass",
            PairReason::Masking => "a masking finding names them",
            PairReason::CoVaries => "their levels rise and fall together",
        }
    }
}

/// A pair to compare (`a` before `b` in the analysis' track order).
#[derive(Clone, Debug, PartialEq)]
pub struct PairCandidate {
    pub a: TrackId,
    pub b: TrackId,
    pub reasons: Vec<PairReason>,
    /// Stretches to prefer for the excerpt (a masking finding's events).
    pub prefer: Vec<TimeSpan>,
    /// The screening's value, when it was computed.
    pub covariation: Option<f64>,
}

impl PairCandidate {
    /// The two share a source by the user's word (roles, selection):
    /// differences between them are less likely to be musical.
    pub fn same_source(&self) -> bool {
        self.reasons.iter().any(|r| {
            matches!(
                r,
                PairReason::SameSource | PairReason::Selected | PairReason::SameAudio
            )
        })
    }
}

fn add(
    tracks: &[TrackInput<'_>],
    out: &mut Vec<PairCandidate>,
    (x, y): (TrackId, TrackId),
    why: PairReason,
    prefer: &[TimeSpan],
    covariation: Option<f64>,
) {
    let idx = |t: TrackId| tracks.iter().position(|i| i.track == t);
    let (Some(i), Some(j)) = (idx(x), idx(y)) else {
        return;
    };
    if i == j || !(tracks[i].focus || tracks[j].focus) {
        return;
    }
    let (a, b) = if i < j { (x, y) } else { (y, x) };
    if let Some(c) = out.iter_mut().find(|c| c.a == a && c.b == b) {
        if !c.reasons.contains(&why) {
            c.reasons.push(why);
        }
        c.prefer.extend_from_slice(prefer);
        c.covariation = c.covariation.or(covariation);
    } else {
        out.push(PairCandidate {
            a,
            b,
            reasons: vec![why],
            prefer: prefer.to_vec(),
            covariation,
        });
    }
}

/// The loudest of `ids` (RMS of the range; ties: the first).
fn loudest(tracks: &[TrackInput<'_>], ids: &[TrackId]) -> Option<TrackId> {
    ids.iter()
        .filter_map(|id| tracks.iter().find(|t| t.track == *id))
        .fold(None::<&TrackInput<'_>>, |best, t| match best {
            Some(b) if b.profile.levels.rms >= t.profile.levels.rms => Some(b),
            _ => Some(t),
        })
        .map(|t| t.track)
}

/// Per frame of a profile: whether it plays (within 40 dB of its
/// loudest frame).
fn activity(p: &TrackProfile) -> Vec<bool> {
    let e: Vec<f64> = (0..p.frames.frames())
        .map(|t| (0..p.frames.bands()).map(|b| p.frames.power(t, b)).sum())
        .collect();
    let top = e.iter().copied().fold(0.0, f64::max);
    let floor = (top * 1e-4).max(SILENCE);
    e.iter().map(|v| *v >= floor && *v > SILENCE).collect()
}

/// How much two tracks' band levels rise and fall together: the
/// correlation of their levels (dB) in the broad regions over the frames
/// both play, each region's mean taken out (`None`: under 20 such frames,
/// or steady levels — nothing to correlate).
pub fn covariation(a: &TrackProfile, b: &TrackProfile) -> Option<f64> {
    if a.frames.frames() != b.frames.frames() || a.layout != b.layout {
        return None;
    }
    let broad = BandLayout::broad();
    let regions: Vec<Vec<usize>> = broad
        .bands()
        .iter()
        .map(|r| {
            a.layout
                .bands()
                .iter()
                .enumerate()
                .filter(|(_, x)| x.centre >= r.lo && x.centre < r.hi)
                .map(|(i, _)| i)
                .collect()
        })
        .collect();
    let (aa, ab) = (activity(a), activity(b));
    let frames: Vec<usize> = (0..a.frames.frames())
        .filter(|t| aa[*t] && ab[*t])
        .collect();
    if frames.len() < 20 {
        return None;
    }
    let level = |p: &TrackProfile, t: usize, r: &[usize]| -> f64 {
        power_db(r.iter().map(|b| p.frames.power(t, *b)).sum())
    };
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for r in regions.iter().filter(|r| !r.is_empty()) {
        let xa: Vec<f64> = frames.iter().map(|t| level(a, *t, r)).collect();
        let xb: Vec<f64> = frames.iter().map(|t| level(b, *t, r)).collect();
        // Regions where either is (nearly) silent say nothing.
        if xa.iter().chain(&xb).any(|v| *v <= -90.0) {
            continue;
        }
        let n = xa.len() as f64;
        let (ma, mb) = (xa.iter().sum::<f64>() / n, xb.iter().sum::<f64>() / n);
        for (x, y) in xa.iter().zip(&xb) {
            sxy += (x - ma) * (y - mb);
            sxx += (x - ma).powi(2);
            syy += (y - mb).powi(2);
        }
    }
    // Under a tenth of a dB of movement on average: steady.
    let n = frames.len() as f64;
    if sxx < 0.01 * n || syy < 0.01 * n {
        return None;
    }
    Some((sxy / (sxx * syy).sqrt()).clamp(-1.0, 1.0))
}

/// The pairs to compare (see the module docs), most important first, at
/// most `rule.max_pairs`. `selected`: the tracks the user selected to
/// analyse (empty unless that is the scope).
pub fn candidates(
    tracks: &[TrackInput<'_>],
    findings: &[Finding],
    selected: &[TrackId],
    rule: &PhaseRule,
) -> Vec<PairCandidate> {
    let mut out = Vec::new();
    let present: Vec<TrackId> = tracks.iter().map(|t| t.track).collect();
    // Selected together.
    let sel: Vec<TrackId> = selected
        .iter()
        .copied()
        .filter(|t| present.contains(t))
        .collect();
    if sel.len() >= 2 {
        if sel.len() <= 5 {
            for (i, x) in sel.iter().enumerate() {
                for y in &sel[i + 1..] {
                    add(tracks, &mut out, (*x, *y), PairReason::Selected, &[], None);
                }
            }
        } else if let Some(hub) = loudest(tracks, &sel) {
            for x in sel.iter().filter(|x| **x != hub) {
                add(tracks, &mut out, (hub, *x), PairReason::Selected, &[], None);
            }
        }
    }
    // One source by the roles.
    let with = |role: TrackRole| -> Vec<TrackId> {
        tracks
            .iter()
            .filter(|t| t.role == Some(role))
            .map(|t| t.track)
            .collect()
    };
    let kit: Vec<TrackId> = tracks
        .iter()
        .filter(|t| {
            matches!(
                t.role,
                Some(TrackRole::Kick | TrackRole::Snare | TrackRole::Drums)
            )
        })
        .map(|t| t.track)
        .collect();
    if let Some(hub) = loudest(tracks, &with(TrackRole::Drums)) {
        for x in kit.iter().filter(|x| **x != hub) {
            add(
                tracks,
                &mut out,
                (hub, *x),
                PairReason::SameSource,
                &[],
                None,
            );
        }
    }
    for role in TrackRole::ALL {
        if matches!(
            role,
            TrackRole::Drums | TrackRole::Effects | TrackRole::Other
        ) {
            continue;
        }
        let group = with(role);
        if group.len() < 2 {
            continue;
        }
        if let Some(hub) = loudest(tracks, &group) {
            for x in group.iter().filter(|x| **x != hub) {
                add(
                    tracks,
                    &mut out,
                    (hub, *x),
                    PairReason::SameSource,
                    &[],
                    None,
                );
            }
        }
    }
    for k in with(TrackRole::Kick) {
        for b in with(TrackRole::Bass) {
            add(tracks, &mut out, (k, b), PairReason::KickAndBass, &[], None);
        }
    }
    // Carrying the same audio (a copy may be the other way up, or late).
    for f in findings {
        if f.rule == crate::finding::RuleId::SameAudio
            && let Subject::Tracks { a, b } = f.subject
        {
            add(
                tracks,
                &mut out,
                (a, b),
                PairReason::SameAudio,
                &f.events,
                None,
            );
        }
    }
    // Named by masking findings (most relevant first).
    for f in findings {
        if let Subject::Pair { target, masker } = f.subject {
            add(
                tracks,
                &mut out,
                (target, masker),
                PairReason::Masking,
                &f.events,
                None,
            );
        }
    }
    // Levels moving together (the screening).
    let mut moving: Vec<(f64, TrackId, TrackId)> = Vec::new();
    for (i, x) in tracks.iter().enumerate() {
        for y in &tracks[i + 1..] {
            if !(x.focus || y.focus)
                || out
                    .iter()
                    .any(|c| (c.a, c.b) == (x.track, y.track) || (c.a, c.b) == (y.track, x.track))
            {
                continue;
            }
            if let Some(c) = covariation(x.profile, y.profile).filter(|c| *c >= rule.covary) {
                moving.push((c, x.track, y.track));
            }
        }
    }
    moving.sort_by(|p, q| q.0.total_cmp(&p.0));
    for (c, x, y) in moving {
        add(tracks, &mut out, (x, y), PairReason::CoVaries, &[], Some(c));
    }
    // Most important reason first (stable: the order found within one).
    out.sort_by_key(|c| c.reasons.iter().min().copied());
    out.truncate(rule.max_pairs);
    out
}

/// Where a pair is compared: samples from the analysed range's start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Excerpt {
    pub start: usize,
    pub frames: usize,
    /// Seconds in it both play.
    pub together: f64,
}

/// Each candidate's excerpt (see the module docs; `None`: they play
/// together for less than `rule.min_together_seconds`).
pub fn excerpts<'a>(
    cands: &[PairCandidate],
    profile: &dyn Fn(TrackId) -> Option<&'a TrackProfile>,
    rule: &PhaseRule,
) -> Vec<Option<Excerpt>> {
    let mut active: Vec<(TrackId, Vec<bool>)> = Vec::new();
    for c in cands {
        for t in [c.a, c.b] {
            if !active.iter().any(|(x, _)| *x == t)
                && let Some(p) = profile(t)
            {
                active.push((t, activity(p)));
            }
        }
    }
    let frames = active.first().map_or(0, |(_, a)| a.len());
    let busy: Vec<f64> = (0..frames)
        .map(|t| {
            active
                .iter()
                .filter(|(_, a)| a.get(t) == Some(&true))
                .count() as f64
        })
        .collect();
    cands
        .iter()
        .map(|c| {
            let (pa, pb) = (profile(c.a)?, profile(c.b)?);
            let aa = &active.iter().find(|(t, _)| *t == c.a)?.1;
            let ab = &active.iter().find(|(t, _)| *t == c.b)?.1;
            let n = aa.len().min(ab.len()).min(pb.frames.frames());
            if n == 0 {
                return None;
            }
            let hop_s = pa.hop_seconds();
            let w = ((rule.excerpt_seconds / hop_s).round() as usize).clamp(1, n);
            let score: Vec<f64> = (0..n)
                .map(|t| {
                    let joint = f64::from(u8::from(aa[t] && ab[t]));
                    let (x, y) = pa.frame_span(t);
                    let preferred = c
                        .prefer
                        .iter()
                        .any(|s| s.overlap(&TimeSpan::new(x, y)) > 0.0);
                    joint * if preferred { 2000.0 } else { 1000.0 }
                        + busy.get(t).copied().unwrap_or(0.0)
                })
                .collect();
            let mut sum: f64 = score[..w].iter().sum();
            let (mut best, mut best_at) = (sum, 0);
            for s in 1..=n - w {
                sum += score[s + w - 1] - score[s - 1];
                if sum > best + 1e-9 {
                    best = sum;
                    best_at = s;
                }
            }
            let together =
                (best_at..best_at + w).filter(|t| aa[*t] && ab[*t]).count() as f64 * hop_s;
            if together < rule.min_together_seconds {
                return None;
            }
            let start = best_at * pa.hop;
            let frames = ((w - 1) * pa.hop + pa.fft_size).min(pa.length.saturating_sub(start));
            Some(Excerpt {
                start,
                frames,
                together,
            })
        })
        .collect()
}

/// What a pair's measurement depends on besides the audio (part of its
/// cache key).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairSettings {
    /// The furthest delay searched (ms).
    pub max_lag_ms: f64,
    /// Onsets this close (ms) are paired.
    pub match_ms: f64,
}

impl PairSettings {
    pub fn of(rules: &crate::config::Rules) -> Self {
        Self {
            max_lag_ms: rules.phase.max_lag_ms,
            match_ms: rules.timing.match_ms,
        }
    }

    pub fn fingerprint(&self) -> u64 {
        let mut h = crate::Fnv::new();
        h.write_u64(self.max_lag_ms.to_bits());
        h.write_u64(self.match_ms.to_bits());
        h.write_u64(u64::from(crate::ALGORITHM_VERSION));
        h.finish()
    }
}

/// The delay estimator's answer (positive: `b` later than `a`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LagEstimate {
    pub samples: f64,
    pub ms: f64,
    pub inverted: bool,
    pub confidence: f64,
    pub clarity: f64,
    /// Above the estimator's own floor (some common signal).
    pub related: bool,
}

/// One band of a pair (powers are mean-square band powers per frame, as
/// [`crate::profile`] measures them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandPhase {
    pub centre: f64,
    pub lo: f64,
    pub hi: f64,
    /// Each track's power (the mean of its channels).
    pub power_a: f64,
    pub power_b: f64,
    /// Twice this is what their sum holds beyond the two powers (the
    /// mean over channels of `Re(A · conj B)`): positive adds, negative
    /// cancels.
    pub cross: f64,
    /// The same with the measured delay and polarity corrected (`None`:
    /// no delay measured).
    pub cross_aligned: Option<f64>,
    /// The same with the median onset offset taken out (`None`: no onsets
    /// paired).
    pub cross_onsets: Option<f64>,
    /// The mono fold-downs' powers and cross term.
    pub mono_a: f64,
    pub mono_b: f64,
    pub mono_cross: f64,
    /// Magnitude-squared coherence of the mono fold-downs (0 to 1,
    /// debiased, power-weighted over the band's bins).
    pub coherence: f64,
    /// The cross-spectrum's phase (radians, `b` against `a`).
    pub phase: f64,
    /// …once the measured delay and polarity are taken out (`None`: no
    /// delay measured).
    pub residual: Option<f64>,
    /// …once the median onset offset is taken out (`None`: no onsets
    /// paired).
    pub residual_onsets: Option<f64>,
}

fn ratio_db(sum: f64, parts: f64) -> Option<f64> {
    (parts > 0.0).then(|| power_db(sum.max(0.0)) - power_db(parts))
}

impl BandPhase {
    /// The sum against the energies now (dB; see the module docs).
    pub fn sum_db(&self) -> Option<f64> {
        let p = self.power_a + self.power_b;
        ratio_db(p + 2.0 * self.cross, p)
    }

    /// …with the delay and polarity corrected.
    pub fn aligned_db(&self) -> Option<f64> {
        let p = self.power_a + self.power_b;
        self.cross_aligned.and_then(|c| ratio_db(p + 2.0 * c, p))
    }

    /// …with the median onset offset taken out.
    pub fn onsets_aligned_db(&self) -> Option<f64> {
        let p = self.power_a + self.power_b;
        self.cross_onsets.and_then(|c| ratio_db(p + 2.0 * c, p))
    }

    /// …with `b`'s polarity flipped.
    pub fn flipped_db(&self) -> Option<f64> {
        let p = self.power_a + self.power_b;
        ratio_db(p - 2.0 * self.cross, p)
    }

    /// …in a mono fold-down.
    pub fn mono_db(&self) -> Option<f64> {
        let p = self.mono_a + self.mono_b;
        ratio_db(p + 2.0 * self.mono_cross, p)
    }
}

/// Onsets of the two, paired.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnsetTiming {
    pub onsets_a: usize,
    pub onsets_b: usize,
    /// Pairs: `a`'s onset (seconds from the analysed range's start) and
    /// how much later `b`'s is (ms).
    pub matched: Vec<(f64, f64)>,
    pub median_ms: Option<f64>,
    /// Median absolute deviation from the median (ms).
    pub spread_ms: Option<f64>,
}

impl OnsetTiming {
    /// Matched onsets over the sparser track's onsets.
    pub fn share(&self) -> f64 {
        let n = self.onsets_a.min(self.onsets_b);
        if n == 0 {
            0.0
        } else {
            self.matched.len() as f64 / n as f64
        }
    }
}

/// A pair's measurement (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct PairPhase {
    pub rate: u32,
    /// The excerpt (seconds from the analysed range's start).
    pub excerpt: TimeSpan,
    /// Frames compared (both active) and the seconds they stand for.
    pub frames: usize,
    pub together: f64,
    pub lag: Option<LagEstimate>,
    pub bands: Vec<BandPhase>,
    pub onsets: OnsetTiming,
}

impl PairPhase {
    /// Memory it takes, roughly (bytes).
    pub fn bytes(&self) -> usize {
        256 + self.bands.len() * std::mem::size_of::<BandPhase>() + self.onsets.matched.len() * 16
    }
}

/// Pair onsets of `a` and `b` (frames) one to one, nearest first, within
/// `window` frames.
fn pair_onsets(a: &[i64], b: &[i64], window: i64) -> Vec<(usize, usize)> {
    let mut cand: Vec<(i64, usize, usize)> = Vec::new();
    let mut lo = 0;
    for (i, &x) in a.iter().enumerate() {
        while lo < b.len() && b[lo] < x - window {
            lo += 1;
        }
        for (j, &y) in b.iter().enumerate().skip(lo) {
            if y > x + window {
                break;
            }
            cand.push(((y - x).abs(), i, j));
        }
    }
    cand.sort_unstable();
    let (mut used_a, mut used_b) = (vec![false; a.len()], vec![false; b.len()]);
    let mut out = Vec::new();
    for (_, i, j) in cand {
        if !used_a[i] && !used_b[j] {
            used_a[i] = true;
            used_b[j] = true;
            out.push((i, j));
        }
    }
    out.sort_unstable();
    out
}

/// Onsets of both (the clearer ones), paired (see [`OnsetTiming`]).
pub fn onset_timing(a: &[f32], b: &[f32], rate: u32, match_ms: f64, offset: f64) -> OnsetTiming {
    use faderframe_audio_files::onsets::{detect, strength_threshold};
    let keep = strength_threshold(0.5);
    let of = |x: &[f32]| -> Vec<i64> {
        detect(x, rate)
            .into_iter()
            .filter(|o| o.strength >= keep)
            .map(|o| o.frame)
            .collect()
    };
    let (oa, ob) = (of(a), of(b));
    let fr = f64::from(rate.max(1));
    let window = (match_ms / 1000.0 * fr).round() as i64;
    let matched: Vec<(f64, f64)> = pair_onsets(&oa, &ob, window)
        .into_iter()
        .map(|(i, j)| {
            (
                offset + oa[i] as f64 / fr,
                (ob[j] - oa[i]) as f64 / fr * 1000.0,
            )
        })
        .collect();
    let mut d: Vec<f64> = matched.iter().map(|m| m.1).collect();
    let median_ms = crate::dynamics::median(&mut d);
    let spread_ms = median_ms.and_then(|m| {
        let mut dev: Vec<f64> = matched.iter().map(|x| (x.1 - m).abs()).collect();
        crate::dynamics::median(&mut dev)
    });
    OnsetTiming {
        onsets_a: oa.len(),
        onsets_b: ob.len(),
        matched,
        median_ms,
        spread_ms,
    }
}

/// Measure a pair (see the module docs): `a` and `b` planar (one or two
/// channels used, equal lengths, at `rate`), `offset` the excerpt's start
/// (seconds from the analysed range's start).
pub fn measure_pair(
    a: &[&[f32]],
    b: &[&[f32]],
    rate: u32,
    config: &AnalysisConfig,
    settings: &PairSettings,
    offset: f64,
    cancel: Option<&AtomicBool>,
) -> Result<PairPhase, PhaseError> {
    config.validate(rate)?;
    let (Some(a0), Some(_)) = (a.first(), b.first()) else {
        return Err(PhaseError::NoChannels);
    };
    let len = a0.len();
    if a.iter().chain(b).any(|c| c.len() != len) {
        return Err(PhaseError::Ragged);
    }
    let stop = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let fr = f64::from(rate);
    let clean = |v: f32| if v.is_finite() { v } else { 0.0 };
    let (ka, kb) = (a.len().min(2), b.len().min(2));
    let chans = ka.max(kb);
    let mono = |x: &[&[f32]], k: usize| -> Vec<f32> {
        (0..len)
            .map(|i| x[..k].iter().map(|c| clean(c[i])).sum::<f32>() / k as f32)
            .collect()
    };
    let (ma, mb) = (mono(a, ka), mono(b, kb));
    let excerpt = TimeSpan::new(offset, offset + len as f64 / fr);
    // The delay and polarity: Auto-Align's estimator.
    let lag = align(
        &[&ma, &mb],
        fr,
        &AlignSettings {
            max_lag: settings.max_lag_ms / 1000.0,
            ..AlignSettings::default()
        },
        None,
    )
    .pair(0, 1)
    .map(|p| LagEstimate {
        samples: p.lag,
        ms: p.lag / fr * 1000.0,
        inverted: p.inverted,
        confidence: f64::from(p.confidence),
        clarity: f64::from(p.clarity),
        related: p.related(),
    });
    if stop() {
        return Err(PhaseError::Cancelled);
    }
    let onsets = onset_timing(&ma, &mb, rate, settings.match_ms, offset);
    // Frames both are active in.
    let n = config.fft_size;
    let hop = n / 2;
    let count = if len <= n {
        usize::from(len > 0)
    } else {
        (len - n) / hop + 1
    };
    let energy = |m: &[f32]| -> Vec<f64> {
        (0..count)
            .map(|f| {
                let s = (f * hop).min(len);
                let e = (s + n).min(len);
                m[s..e].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / n as f64
            })
            .collect()
    };
    let active = |e: &[f64]| -> Vec<bool> {
        let top = e.iter().copied().fold(0.0, f64::max);
        let floor = (top * ACTIVE).max(SILENCE);
        e.iter().map(|v| *v >= floor && *v > SILENCE).collect()
    };
    let (aa, ab) = (active(&energy(&ma)), active(&energy(&mb)));
    let mut both: Vec<usize> = (0..count).filter(|f| aa[*f] && ab[*f]).collect();
    if both.len() > MAX_FRAMES {
        let step = both.len() as f64 / MAX_FRAMES as f64;
        both = (0..MAX_FRAMES)
            .map(|i| both[(i as f64 * step) as usize])
            .collect();
    }
    let k_frames = both.len();
    let together = k_frames as f64 * hop as f64 / fr;
    if k_frames < 2 {
        return Ok(PairPhase {
            rate,
            excerpt,
            frames: k_frames,
            together,
            lag,
            bands: Vec::new(),
            onsets,
        });
    }
    // Averaged spectra per bin.
    let half = n / 2;
    let window: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos())
        .collect();
    let s2: f64 = window.iter().map(|w| w * w).sum();
    let norm = 1.0 / (n as f64 * s2.max(1e-12));
    let bins = half + 1;
    let (mut paa, mut pbb) = (vec![0.0; bins], vec![0.0; bins]);
    let (mut gre, mut gim) = (vec![0.0; bins], vec![0.0; bins]);
    let (mut mpa, mut mpb) = (vec![0.0; bins], vec![0.0; bins]);
    let (mut mre, mut mim) = (vec![0.0; bins], vec![0.0; bins]);
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    let mut fold = vec![(0.0f64, 0.0f64, 0.0f64, 0.0f64); bins];
    let c = chans as f64;
    for (i, &f) in both.iter().enumerate() {
        if i % 16 == 0 && stop() {
            return Err(PhaseError::Cancelled);
        }
        fold.fill((0.0, 0.0, 0.0, 0.0));
        for ch in 0..chans {
            let (xa, xb) = (a[ch.min(ka - 1)], b[ch.min(kb - 1)]);
            for (j, w) in window.iter().enumerate() {
                let at = f * hop + j;
                re[j] = xa.get(at).map_or(0.0, |v| f64::from(clean(*v))) * w;
                im[j] = xb.get(at).map_or(0.0, |v| f64::from(clean(*v))) * w;
            }
            faderframe_analysis::fft(&mut re, &mut im);
            for k in 0..bins {
                let nk = (n - k) % n;
                let (zr, zi, cr, ci) = (re[k], im[k], re[nk], -im[nk]);
                let (ar, ai) = ((zr + cr) * 0.5, (zi + ci) * 0.5);
                let (br, bi) = ((zi - ci) * 0.5, -(zr - cr) * 0.5);
                paa[k] += (ar * ar + ai * ai) / c;
                pbb[k] += (br * br + bi * bi) / c;
                // conj(A) · B.
                gre[k] += (ar * br + ai * bi) / c;
                gim[k] += (ar * bi - ai * br) / c;
                let x = &mut fold[k];
                x.0 += ar / c;
                x.1 += ai / c;
                x.2 += br / c;
                x.3 += bi / c;
            }
        }
        for (k, &(ar, ai, br, bi)) in fold.iter().enumerate() {
            mpa[k] += ar * ar + ai * ai;
            mpb[k] += br * br + bi * bi;
            mre[k] += ar * br + ai * bi;
            mim[k] += ar * bi - ai * br;
        }
    }
    let layout = config.layout(rate);
    let weights = layout.bin_weights(n, fr);
    let kf = k_frames as f64;
    let scale = |k: usize| {
        if k == 0 || k == half {
            norm / kf
        } else {
            2.0 * norm / kf
        }
    };
    let rot = lag.filter(|l| l.related).map(|l| {
        let sign = if l.inverted { -1.0 } else { 1.0 };
        (l.samples, sign)
    });
    // The onsets' median offset (samples, `b` later).
    let shift = onsets.median_ms.map(|m| m / 1000.0 * fr);
    let bands = layout
        .bands()
        .iter()
        .enumerate()
        .map(|(bi, band)| {
            let mut x = BandPhase {
                centre: band.centre,
                lo: band.lo,
                hi: band.hi,
                power_a: 0.0,
                power_b: 0.0,
                cross: 0.0,
                cross_aligned: rot.map(|_| 0.0),
                cross_onsets: shift.map(|_| 0.0),
                mono_a: 0.0,
                mono_b: 0.0,
                mono_cross: 0.0,
                coherence: 0.0,
                phase: 0.0,
                residual: None,
                residual_onsets: None,
            };
            let (mut pr, mut pi) = (0.0, 0.0);
            let (mut rr, mut ri) = (0.0, 0.0);
            let (mut or, mut oi) = (0.0, 0.0);
            let (mut cn, mut cd) = (0.0, 0.0);
            for &(k, w) in weights.band(bi) {
                let s = scale(k) * f64::from(w);
                x.power_a += s * paa[k];
                x.power_b += s * pbb[k];
                x.cross += s * gre[k];
                x.mono_a += s * mpa[k];
                x.mono_b += s * mpb[k];
                x.mono_cross += s * mre[k];
                pr += s * mre[k];
                pi += s * mim[k];
                if let Some((tau, sign)) = rot {
                    let wt = TAU * k as f64 / n as f64 * tau;
                    let (co, si) = (wt.cos(), wt.sin());
                    if let Some(ca) = &mut x.cross_aligned {
                        *ca += s * sign * (gre[k] * co - gim[k] * si);
                    }
                    rr += s * sign * (mre[k] * co - mim[k] * si);
                    ri += s * sign * (mre[k] * si + mim[k] * co);
                }
                if let (Some(tau), Some(co)) = (shift, &mut x.cross_onsets) {
                    let wt = TAU * k as f64 / n as f64 * tau;
                    let (c, si) = (wt.cos(), wt.sin());
                    *co += s * (gre[k] * c - gim[k] * si);
                    or += s * (mre[k] * c - mim[k] * si);
                    oi += s * (mre[k] * si + mim[k] * c);
                }
                let den = mpa[k] * mpb[k];
                if den > 0.0 {
                    let msc = ((mre[k] * mre[k] + mim[k] * mim[k]) / den).min(1.0);
                    let msc = if k_frames >= 4 {
                        ((msc - 1.0 / kf) / (1.0 - 1.0 / kf)).clamp(0.0, 1.0)
                    } else {
                        msc
                    };
                    let wt = s * den.sqrt();
                    cn += wt * msc;
                    cd += wt;
                }
            }
            x.coherence = if cd > 0.0 { cn / cd } else { 0.0 };
            x.phase = pi.atan2(pr);
            x.residual = rot.map(|_| ri.atan2(rr));
            x.residual_onsets = shift.map(|_| oi.atan2(or));
            x
        })
        .collect();
    Ok(PairPhase {
        rate,
        excerpt,
        frames: k_frames,
        together,
        lag,
        bands,
        onsets,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::SR;

    /// A deterministic noise source.
    pub(crate) struct Noise(pub u64);

    impl Noise {
        pub(crate) fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        }
    }

    /// Drum-like hits (a falling sine with a click, then noise bursts),
    /// every `beat` seconds over `seconds`.
    pub(crate) fn drums(seconds: f64, beat: f64, seed: u64) -> Vec<f32> {
        let rate = f64::from(SR);
        let len = (seconds * rate) as usize;
        let mut x = vec![0.0f32; len];
        let mut noise = Noise(seed);
        let step = (beat * rate) as usize;
        let mut at = 0;
        let mut i = 0;
        while at < len {
            let vel = 0.6 + 0.4 * noise.next().abs();
            for k in 0..(rate * 0.3) as usize {
                if at + k >= len {
                    break;
                }
                let s = k as f64 / rate;
                let v = if i % 2 == 0 {
                    let f = 50.0 + 90.0 * (-s * 30.0).exp();
                    (TAU * f * s).sin() * (-s * 12.0).exp()
                        + 0.3 * noise.next() * (-s * 400.0).exp()
                } else {
                    noise.next() * 0.6 * (-s * 25.0).exp()
                        + 0.4 * (TAU * 190.0 * s).sin() * (-s * 30.0).exp()
                };
                x[at + k] += (0.5 * vel * v) as f32;
            }
            at += step;
            i += 1;
        }
        x
    }

    /// `x` later by `d` samples (fractional: windowed sinc, 64 taps).
    pub(crate) fn delayed(x: &[f32], d: f64) -> Vec<f32> {
        let whole = d.floor();
        let frac = d - whole;
        let taps = 32i64;
        (0..x.len() as i64)
            .map(|i| {
                if frac.abs() < 1e-12 {
                    let j = i - whole as i64;
                    return if j >= 0 { x[j as usize] } else { 0.0 };
                }
                let mut acc = 0.0f64;
                for t in -taps + 1..=taps {
                    let j = i - whole as i64 - t;
                    if j < 0 || j >= x.len() as i64 {
                        continue;
                    }
                    let u = t as f64 - frac;
                    let sinc = if u.abs() < 1e-12 {
                        1.0
                    } else {
                        (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u)
                    };
                    let w = 0.5 + 0.5 * (std::f64::consts::PI * u / taps as f64).cos();
                    acc += f64::from(x[j as usize]) * sinc * w;
                }
                acc as f32
            })
            .collect()
    }

    pub(crate) fn measure(a: &[&[f32]], b: &[&[f32]]) -> PairPhase {
        let config = AnalysisConfig::for_rate(SR);
        measure_pair(
            a,
            b,
            SR,
            &config,
            &PairSettings {
                max_lag_ms: 30.0,
                match_ms: 50.0,
            },
            0.0,
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    /// The relevant bands' broadband sum against the energies (dB).
    fn broadband(p: &PairPhase, f: impl Fn(&BandPhase) -> f64) -> f64 {
        let (mut s, mut e) = (0.0, 0.0);
        for b in p
            .bands
            .iter()
            .filter(|b| b.power_a > 1e-9 && b.power_b > 1e-9)
        {
            s += b.power_a + b.power_b + 2.0 * f(b);
            e += b.power_a + b.power_b;
        }
        10.0 * (s / e).log10()
    }

    fn lag(p: &PairPhase) -> LagEstimate {
        p.lag.unwrap_or(LagEstimate {
            samples: f64::NAN,
            ms: f64::NAN,
            inverted: false,
            confidence: 0.0,
            clarity: 0.0,
            related: false,
        })
    }

    #[test]
    fn identical_signals_add_in_phase() {
        let x = drums(8.0, 0.25, 7);
        let p = measure(&[&x], &[&x]);
        let l = lag(&p);
        assert!(l.related && !l.inverted, "{l:?}");
        assert!(l.samples.abs() < 0.01, "{l:?}");
        assert!((broadband(&p, |b| b.cross) - 3.01).abs() < 0.05);
        for b in p.bands.iter().filter(|b| b.power_a > 1e-7) {
            assert!(b.coherence > 0.95, "{} Hz {}", b.centre, b.coherence);
            assert!(b.phase.abs() < 0.05);
            assert!((b.sum_db().unwrap_or(0.0) - 3.01).abs() < 0.05);
            assert!(b.flipped_db().unwrap_or(0.0) < -60.0, "flipped cancels");
        }
        assert!(p.together > 7.0);
    }

    #[test]
    fn an_inverted_copy_cancels_and_is_told_from_a_delay() {
        let x = drums(8.0, 0.25, 8);
        let inv: Vec<f32> = x.iter().map(|v| -v).collect();
        let p = measure(&[&x], &[&inv]);
        let l = lag(&p);
        assert!(l.related && l.inverted && l.samples.abs() < 0.01, "{l:?}");
        assert!(broadband(&p, |b| b.cross) < -60.0, "cancels in the sum");
        for b in p.bands.iter().filter(|b| b.power_a > 1e-7) {
            assert!((b.phase.abs() - std::f64::consts::PI).abs() < 0.05);
            // Corrected (flipped back): in phase again, nothing left over.
            assert!((b.aligned_db().unwrap_or(0.0) - 3.01).abs() < 0.05);
            assert!(b.residual.unwrap_or(9.0).abs() < 0.05);
        }
    }

    #[test]
    fn known_and_fractional_delays_are_found_with_their_comb() {
        let x = drums(8.0, 0.25, 9);
        for d in [24.0, 10.4, 0.6] {
            let y = delayed(&x, d);
            let p = measure(&[&x], &[&y]);
            let l = lag(&p);
            assert!(l.related && !l.inverted, "{d}: {l:?}");
            assert!((l.samples - d).abs() < 0.05, "{d}: {l:?}");
            assert!((l.ms - d / 48.0).abs() < 0.002);
            // Corrected: every band back in phase.
            for b in p
                .bands
                .iter()
                .filter(|b| b.power_a > 1e-7 && b.hi < 16_000.0)
            {
                let al = b.aligned_db().unwrap_or(0.0);
                assert!((al - 3.01).abs() < 0.2, "{d}: {} Hz {al}", b.centre);
                assert!(
                    b.residual.unwrap_or(9.0).abs() < 0.2,
                    "{d}: {} Hz",
                    b.centre
                );
            }
        }
        // 24 samples (0.5 ms): the comb's first notch at 1 kHz.
        let p = measure(&[&x], &[&delayed(&x, 24.0)]);
        let at = |hz: f64| {
            p.bands
                .iter()
                .find(|b| b.lo <= hz && b.hi > hz)
                .and_then(BandPhase::sum_db)
                .unwrap_or(0.0)
        };
        assert!(at(1000.0) < -10.0, "{}", at(1000.0));
        assert!(at(100.0) > 2.5, "below the notch they add: {}", at(100.0));
        assert!(at(2000.0) > 2.0, "the comb's next peak: {}", at(2000.0));
        // In mono (a mono pair): the same.
        let b = p.bands.iter().find(|b| b.lo <= 1000.0 && b.hi > 1000.0);
        assert!(b.and_then(BandPhase::mono_db).unwrap_or(0.0) < -10.0);
    }

    #[test]
    fn unrelated_signals_neither_add_nor_cancel() {
        let mut n1 = Noise(1);
        let mut n2 = Noise(2);
        let a: Vec<f32> = (0..SR as usize * 8)
            .map(|_| (0.3 * n1.next()) as f32)
            .collect();
        let b: Vec<f32> = (0..SR as usize * 8)
            .map(|_| (0.3 * n2.next()) as f32)
            .collect();
        let p = measure(&[&a], &[&b]);
        assert!(!lag(&p).related, "{:?}", p.lag);
        assert!(broadband(&p, |b| b.cross).abs() < 0.2);
        for b in p.bands.iter().filter(|b| b.centre > 100.0) {
            assert!(b.coherence < 0.1, "{} Hz {}", b.centre, b.coherence);
            assert!(b.sum_db().unwrap_or(9.0).abs() < 1.0, "{} Hz", b.centre);
        }
        // Silence on one side: nothing compared, no error.
        let z = vec![0.0f32; a.len()];
        let p = measure(&[&a], &[&z]);
        assert_eq!(p.frames, 0);
        assert!(p.bands.is_empty() && p.lag.is_none());
        // Bad input.
        let config = AnalysisConfig::for_rate(SR);
        let s = PairSettings {
            max_lag_ms: 30.0,
            match_ms: 50.0,
        };
        assert_eq!(
            measure_pair(&[], &[&a], SR, &config, &s, 0.0, None).err(),
            Some(PhaseError::NoChannels)
        );
        assert_eq!(
            measure_pair(&[&a], &[&a[..100]], SR, &config, &s, 0.0, None).err(),
            Some(PhaseError::Ragged)
        );
        let stop = AtomicBool::new(true);
        assert_eq!(
            measure_pair(&[&a], &[&a], SR, &config, &s, 0.0, Some(&stop)).err(),
            Some(PhaseError::Cancelled)
        );
    }

    #[test]
    fn onsets_show_flams_and_loose_doubles() {
        let x = drums(8.0, 0.5, 11);
        // Another performance of the same part 12 ms late.
        let y = delayed(&drums(8.0, 0.5, 12), 0.012 * 48_000.0);
        let t = onset_timing(&x, &y, SR, 50.0, 2.0);
        assert!(t.matched.len() >= 12, "{t:?}");
        assert!(t.share() > 0.8);
        let m = t.median_ms.unwrap_or(0.0);
        assert!((m - 12.0).abs() < 1.5, "{m}");
        assert!(t.spread_ms.unwrap_or(9.0) < 1.5, "{t:?}");
        assert!(t.matched[0].0 >= 2.0, "times from the range's start");
        // Loose: every hit moved by its own ±15 ms.
        let mut jitter = Noise(5);
        let rate = f64::from(SR);
        let mut loose = vec![0.0f32; x.len()];
        let hit = drums(0.5, 1.0, 13);
        for k in 0..16 {
            let at = (k as f64 * 0.5 + 0.015 * jitter.next()) * rate;
            let at = at.max(0.0) as usize;
            for (i, v) in hit.iter().enumerate() {
                if at + i < loose.len() {
                    loose[at + i] += v;
                }
            }
        }
        let t = onset_timing(&x, &loose, SR, 50.0, 0.0);
        assert!(t.matched.len() >= 10, "{t:?}");
        assert!(t.spread_ms.unwrap_or(0.0) > 4.0, "{t:?}");
        // Pairing is one to one, nearest first.
        assert_eq!(pair_onsets(&[0, 100], &[5, 90, 300], 20), [(0, 0), (1, 1)]);
    }
}
