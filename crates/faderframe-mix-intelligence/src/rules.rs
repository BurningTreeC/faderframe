//! The diagnostic rules: measurements in, findings out (ranked).
//!
//! Every rule is explicit: what it reads, when it applies, its thresholds
//! ([`crate::config::Rules`]), how it scores, what it cannot tell
//! ([`RuleId::limitations`]). Severity and confidence are kept apart:
//! severity is how much the measured interaction weighs (how long, how
//! close, how much of the source), confidence how strong and complete the
//! evidence for the interpretation is (which signal was measured, whether
//! the roles are known, how much there is of it). Both are 0 to 1, shown
//! in three grades with the terms they came from.
//!
//! # Spectral masking ([`RuleId::SpectralMasking`])
//!
//! Reads two tracks' profiles ([`crate::masking::conflicts`]), both
//! directions where the roles allow (see [`pair_weight`]); per unordered
//! pair and overlapping range only the stronger direction is kept.
//! Stretches where the two carry the same audio ([`crate::same`]: copies,
//! one source processed two ways) are left out, window by window, and
//! reported instead ([`crate::rules_same`], [`RuleId::SameAudio`]).
//!
//! * severity = role weight × √persistence × depth × importance, where
//!   persistence is the share of the target's time in the range it is
//!   covered, depth the mean coverage while covered (0.5 to 1), importance
//!   `clamp(share / 0.25, 0.25, 1)` (a range holding a quarter or more of
//!   the target's energy counts fully);
//! * confidence = signal (processed 1, source 0.6) × roles (both known 1,
//!   one 0.85, none 0.7) × amount (`0.6 + 0.4 · min(1, covered seconds /
//!   8)`) × level (1 when the target sits 20 dB or more above the floor
//!   there, else 0.7) × resolution (0.85 for ranges reaching under 40 Hz);
//! * reported from a severity of [`crate::config::MaskingRule::min_severity`].
//!
//! # Levels
//!
//! [`RuleId::MasterTruePeak`] (true peak over the ceiling; severity
//! `0.3 + 0.2 × dB over`, at most 1), [`RuleId::ClippedSamples`] (overs on
//! the master; `0.3 + 0.1 × log2(1 + overs)`), [`RuleId::OverFullScale`]
//! (a processed track alone above 0 dBFS at the master; `0.25 + 0.1 × dB
//! over`), [`RuleId::NonFiniteSamples`] (0.9), [`RuleId::SectionLoudnessStep`]
//! (adjacent sections of the master; 0.2 — a step is often intended).
//!
//! # Stereo
//!
//! [`RuleId::MonoFoldDownLoss`]: bands within `relevance_db` of the
//! loudest, losing more than `fold_loss_db` in a mono fold-down (half that
//! under `low_end_hz`) for at least `min_seconds`, grouped into ranges;
//! severity `clamp(loss / 12, 0.15, 0.9)`, × 0.7 above the low end.
//! [`RuleId::InvertedChannel`]: broadband correlation at or under
//! `inverted_correlation` over frames where both channels carry the signal
//! within 6 dB of each other, for at least `min_seconds`; severity 0.8.

use crate::bands::BandLayout;
use crate::config::Rules;
use crate::finding::{
    Category, Evidence, Experiment, Finding, FrequencyRange, Pattern, Recommendation, RuleId,
    SignalPoint, SpectrumEvidence, Subject, Term, TimeSpan, Unit, labels,
};
use crate::masking::{ConflictRange, MaskingError};
use crate::profile::{TrackProfile, power_db};
use crate::stereo;
use faderframe_core::{RoleFamily, TrackId, TrackRole};
use std::sync::atomic::{AtomicBool, Ordering};

/// One track to diagnose.
pub struct TrackInput<'a> {
    pub track: TrackId,
    pub role: Option<TrackRole>,
    pub profile: &'a TrackProfile,
    /// Compared with the other tracks for masking (sources, not buses).
    pub compare: bool,
    /// What the analysis is about: pairs need at least one track in focus,
    /// the level and stereo rules look at tracks in focus only.
    pub focus: bool,
}

/// What [`diagnose`] reads.
pub struct DiagnoseInput<'a> {
    pub tracks: Vec<TrackInput<'a>>,
    /// The master's profile (its track, when known): level and stereo
    /// rules; its `span_loudness` follows `sections`.
    pub master: Option<(Option<TrackId>, &'a TrackProfile)>,
    pub signal: SignalPoint,
    /// Named stretches (seconds from the start of the range).
    pub sections: Vec<(String, TimeSpan)>,
    pub rules: &'a Rules,
    pub cancel: Option<&'a AtomicBool>,
    /// Pairs never compared for masking (either order): a track and a bus
    /// or return its signal reaches (that one carries it).
    pub related: Vec<(TrackId, TrackId)>,
}

/// One subject's levels and stereo picture (facts, no judgement).
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub subject: Subject,
    pub sample_peak: f64,
    pub true_peak: f64,
    pub rms: f64,
    /// Sample peak minus RMS (dB).
    pub crest: f64,
    /// BS.1770 integrated loudness and range (`None`: silent or too short).
    pub integrated: Option<f64>,
    pub loudness_range: Option<f64>,
    pub max_short_term: Option<f64>,
    /// Power-weighted broadband correlation of left and right.
    pub correlation: f64,
    /// Levels of the broad regions ([`crate::bands::BROAD_NAMES`], dB).
    pub broad: Vec<f64>,
    /// Integrated loudness per section (master only).
    pub sections: Vec<(String, Option<f64>)>,
}

/// Findings (most relevant first) and summaries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub summaries: Vec<Summary>,
    /// Dynamics, width, pairs compared and charts ([`crate::details`]).
    pub details: crate::details::Details,
}

impl Report {
    /// Add findings (made by other rules over the same analysis) and rank
    /// everything again (most relevant first, ties by id); a finding whose
    /// id is already there is left out.
    pub fn add(&mut self, findings: Vec<Finding>) {
        for f in findings {
            if f.severity.is_finite()
                && f.confidence.is_finite()
                && !self.findings.iter().any(|x| x.id == f.id)
            {
                self.findings.push(f);
            }
        }
        self.findings.sort_by(|a, b| {
            b.relevance()
                .total_cmp(&a.relevance())
                .then(a.id.cmp(&b.id))
        });
    }
}

impl Report {
    /// Findings of other rules (the health check, a reference) ranked in
    /// with these.
    pub fn merge(&mut self, more: Vec<Finding>) {
        self.findings.extend(
            more.into_iter()
                .filter(|f| f.severity.is_finite() && f.confidence.is_finite()),
        );
        rank(&mut self.findings);
    }
}

/// Most relevant first (ties by id: the same order every time).
pub fn rank(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        b.relevance()
            .total_cmp(&a.relevance())
            .then(a.id.cmp(&b.id))
    });
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("the diagnosis was cancelled")]
pub struct Cancelled;

impl From<MaskingError> for Cancelled {
    fn from(_: MaskingError) -> Self {
        Cancelled
    }
}

/// FNV-1a of the parts: finding ids.
fn id_of(rule: RuleId, signal: SignalPoint, parts: &[u64]) -> u64 {
    let mut h = crate::Fnv::new();
    h.write_u64(rule as u64);
    h.write_u64(signal as u64);
    for p in parts {
        h.write_u64(*p);
    }
    h.finish()
}

/// How much a target/masker pair weighs by their roles, or `None` when it
/// is not compared (parts of one drum kit: several microphones on one
/// instrument). Unknown roles: 0.8 (neutral).
pub fn pair_weight(target: Option<TrackRole>, masker: Option<TrackRole>) -> Option<f64> {
    use RoleFamily::*;
    let (Some(t), Some(m)) = (target, masker) else {
        return Some(0.8);
    };
    let (ft, fm) = (t.family(), m.family());
    Some(match (ft, fm) {
        (DrumKit, DrumKit) => return None,
        (Foreground, Foreground) => 0.9,
        (Foreground, _) => 1.0,
        _ if matches!(
            (t, m),
            (TrackRole::Kick, TrackRole::Bass) | (TrackRole::Bass, TrackRole::Kick)
        ) =>
        {
            1.0
        }
        (Effects, _) => 0.5,
        (_, Foreground) => 0.6,
        (_, Effects) => 0.7,
        (LowEnd, Accompaniment) => 0.9,
        (Accompaniment, Accompaniment) => 0.85,
        (DrumKit, _) | (_, DrumKit) => 0.8,
        _ => 0.8,
    })
}

fn round_to(v: f64, step: f64) -> f64 {
    (v / step).round() * step
}

/// A bell's Q for a range `octaves` wide (`√(2^N) / (2^N − 1)`), kept
/// within 0.7 to 4.
pub fn q_for_octaves(octaves: f64) -> f64 {
    let n = 2f64.powf(octaves.max(0.05));
    (n.sqrt() / (n - 1.0)).clamp(0.7, 4.0)
}

fn masking_recommendations(
    target: TrackId,
    masker: TrackId,
    roles: (Option<TrackRole>, Option<TrackRole>),
    r: &ConflictRange,
) -> Vec<Recommendation> {
    let range = FrequencyRange {
        lo: r.lo_hz,
        hi: r.hi_hz,
    };
    let freq = round_to(range.centre(), 1.0);
    let q = round_to(q_for_octaves(range.octaves()), 0.1);
    // Deeper when the masker is louder: 2 dB with the masker 6 dB under,
    // 3.5 dB at the same level, at most 4 dB.
    let cut = -round_to((2.0 + 0.25 * (r.margin_db() + 6.0)).clamp(1.5, 4.0), 0.5);
    let label = range.label();
    let stat = Recommendation {
        experiment: Experiment::EqCut {
            track: masker,
            freq_hz: freq,
            gain_db: cut,
            q,
        },
        purpose: format!(
            "Lower the other track's energy at {label}, where it covers the target, to let the \
             target through."
        ),
        side_effects: "A static cut thins the track there all the time, also where nothing \
                       competes with it; the change may not be wanted on its own."
            .into(),
    };
    let dynamic = Recommendation {
        experiment: Experiment::DynamicEqCut {
            track: masker,
            key: target,
            freq_hz: freq,
            range_db: cut,
            q,
        },
        purpose: format!(
            "Cut {label} only while the target plays there (the EQ's band keyed from the \
             target), leaving the track as it is elsewhere."
        ),
        side_effects: "The cut follows the target's level with the EQ's automatic threshold \
                       and timing: it can be heard moving (pumping), and a sustained target \
                       holds it down."
            .into(),
    };
    let level = Recommendation {
        experiment: Experiment::Level {
            track: target,
            change_db: 1.0,
        },
        purpose: "Compare with the target 1 dB louder instead of changing the other track's \
                  tone."
            .into(),
        side_effects: "Changes the balance and the loudness of the whole mix everywhere the \
                       target plays; compare loudness-matched."
            .into(),
    };
    let mut out = if r.pattern == Pattern::Persistent {
        vec![stat, dynamic, level]
    } else {
        vec![dynamic, stat, level]
    };
    if matches!(
        roles,
        (Some(TrackRole::Kick), Some(TrackRole::Bass))
            | (Some(TrackRole::Bass), Some(TrackRole::Kick))
    ) {
        out.push(Recommendation {
            experiment: Experiment::Manual {
                text: "listen to the kick and bass with one of them polarity-inverted, and \
                       check their timing against each other"
                    .into(),
            },
            purpose: "Low-frequency overlap of two related parts can also be partial \
                      cancellation between them, which this analysis does not measure."
                .into(),
            side_effects: "Nothing is changed.".into(),
        });
    }
    out
}

/// Section names most of `events` fall in (in order).
fn sections_of(events: &[TimeSpan], sections: &[(String, TimeSpan)]) -> Vec<String> {
    let total: f64 = events.iter().map(TimeSpan::seconds).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    sections
        .iter()
        .filter(|(_, s)| {
            let inside: f64 = events.iter().map(|e| e.overlap(s)).sum();
            inside >= 2.0 || inside >= 0.25 * total
        })
        .map(|(n, _)| n.clone())
        .collect()
}

fn masking_finding(
    input: &DiagnoseInput<'_>,
    target: &TrackInput<'_>,
    masker: &TrackInput<'_>,
    weight: f64,
    r: ConflictRange,
) -> Finding {
    let rule = &input.rules.masking;
    let importance = (r.share / 0.25).clamp(0.25, 1.0);
    let severity = (weight * r.persistence.sqrt() * r.depth * importance).clamp(0.0, 1.0);
    let signal = match input.signal {
        SignalPoint::Processed => 1.0,
        SignalPoint::Source => 0.6,
    };
    let roles = match (target.role.is_some(), masker.role.is_some()) {
        (true, true) => 1.0,
        (false, false) => 0.7,
        _ => 0.85,
    };
    let amount = 0.6 + 0.4 * (r.conflict_seconds / 8.0).min(1.0);
    let level = if r.target_db - rule.floor_db >= 20.0 {
        1.0
    } else {
        0.7
    };
    let resolution = if r.lo_hz < 40.0 { 0.85 } else { 1.0 };
    let confidence = (signal * roles * amount * level * resolution).clamp(0.0, 1.0);
    let (t, m) = (target.track, masker.track);
    let mut evidence = vec![
        Evidence::new(
            labels::CONFLICT_SECONDS,
            r.conflict_seconds,
            Unit::Seconds,
            None,
        ),
        Evidence::new(
            labels::ACTIVE_SECONDS,
            r.active_seconds,
            Unit::Seconds,
            Some(t),
        ),
        Evidence::new(labels::PERSISTENCE, r.persistence, Unit::Share, None),
        Evidence::new(labels::EVENTS, r.events.len() as f64, Unit::Count, None),
        Evidence::new(labels::TARGET_LEVEL, r.target_db, Unit::DbBand, Some(t)),
        Evidence::new(labels::MASKER_LEVEL, r.masker_db, Unit::DbBand, Some(m)),
        Evidence::new(labels::MARGIN, r.margin_db(), Unit::Db, None),
        Evidence::new(labels::DEPTH, r.depth, Unit::Share, None),
        Evidence::new(labels::SPATIAL, r.spatial, Unit::Share, None),
        Evidence::new(labels::SHARE, r.share, Unit::Share, Some(t)),
    ];
    let mut caveats = Vec::new();
    if let Some((lo, hi)) = r.wider {
        evidence.push(Evidence::new("Overlap extends from", lo, Unit::Hz, None));
        evidence.push(Evidence::new("Overlap extends to", hi, Unit::Hz, None));
        caveats.push(
            "The overlap is broader than this range; the two octaves covered most are shown."
                .into(),
        );
    }
    if target.role.is_none() || masker.role.is_none() {
        caveats.push(
            "Without roles for both tracks the analysis does not know which one should come \
             through; assign roles to weigh the pair."
                .into(),
        );
    }
    if input.signal == SignalPoint::Source {
        caveats.push(
            "Measured before processing: the mix's EQ, dynamics and levels may already change \
             this."
                .into(),
        );
    }
    let time = match (r.events.first(), r.events.last()) {
        (Some(a), Some(b)) => Some(TimeSpan::new(a.start, b.end)),
        _ => None,
    };
    Finding {
        id: id_of(
            RuleId::SpectralMasking,
            input.signal,
            &[t.0, m.0, r.bands.0 as u64],
        ),
        rule: RuleId::SpectralMasking,
        signal: input.signal,
        subject: Subject::Pair {
            target: t,
            masker: m,
        },
        tracks: vec![t, m],
        time,
        sections: sections_of(&r.events, &input.sections),
        events: r.events.clone(),
        pattern: Some(r.pattern),
        frequency: Some(FrequencyRange {
            lo: r.lo_hz,
            hi: r.hi_hz,
        }),
        severity,
        severity_terms: vec![
            Term {
                name: "role weight",
                value: weight,
            },
            Term {
                name: "√persistence",
                value: r.persistence.sqrt(),
            },
            Term {
                name: "depth",
                value: r.depth,
            },
            Term {
                name: "importance",
                value: importance,
            },
        ],
        confidence,
        confidence_terms: vec![
            Term {
                name: "signal",
                value: signal,
            },
            Term {
                name: "roles",
                value: roles,
            },
            Term {
                name: "amount",
                value: amount,
            },
            Term {
                name: "level",
                value: level,
            },
            Term {
                name: "resolution",
                value: resolution,
            },
        ],
        evidence,
        spectrum: Some(SpectrumEvidence {
            centres: target
                .profile
                .layout
                .bands()
                .iter()
                .map(|b| b.centre)
                .collect(),
            series: vec![
                (t, r.target_spectrum.clone()),
                (m, r.masker_spectrum.clone()),
            ],
            highlight: r.bands,
        }),
        recommendations: masking_recommendations(t, m, (target.role, masker.role), &r),
        caveats,
    }
}

fn overlaps(a: &Finding, b: &Finding) -> bool {
    match (a.frequency, b.frequency) {
        (Some(x), Some(y)) => x.lo < y.hi && y.lo < x.hi,
        _ => false,
    }
}

/// A pair's findings and the charts they show.
type PairFound = (Vec<Finding>, Vec<(u64, crate::details::Chart)>);

/// One pair's masking findings — both directions where the roles allow,
/// per overlapping range only the more severe direction, frames where the
/// two carry the same audio left out — and the same-audio finding (with
/// its chart).
fn pair_masking(
    input: &DiagnoseInput<'_>,
    a: &TrackInput<'_>,
    b: &TrackInput<'_>,
    (sa, sb): (&crate::same::BandSums, &crate::same::BandSums),
) -> Result<PairFound, Cancelled> {
    let rule = &input.rules.masking;
    // Where the two carry the same audio, masking is not judged.
    let same = crate::same::same_audio(a.profile, sa, b.profile, sb, &input.rules.same);
    let mut pair: Vec<Finding> = Vec::new();
    for (target, masker) in [(a, b), (b, a)] {
        let Some(weight) = pair_weight(target.role, masker.role) else {
            continue;
        };
        for r in crate::masking::conflicts_except(
            target.profile,
            masker.profile,
            rule,
            &same.frames,
            input.cancel,
        )? {
            let f = masking_finding(input, target, masker, weight, r);
            if f.severity >= rule.min_severity {
                pair.push(f);
            }
        }
    }
    // One direction per overlapping range: the more severe.
    pair.sort_by(|x, y| y.severity.total_cmp(&x.severity).then(x.id.cmp(&y.id)));
    let mut kept: Vec<Finding> = Vec::new();
    for f in pair {
        if let Some(k) = kept.iter_mut().find(|k| overlaps(k, &f)) {
            if k.subject != f.subject {
                k.caveats.push(format!(
                    "The other way round was found too (severity {:.2}): the two \
                     tracks cover each other there.",
                    f.severity
                ));
            }
            continue;
        }
        kept.push(f);
    }
    if same.any() {
        for k in &mut kept {
            k.caveats.push(format!(
                "For {:.0} s the two carry the same audio (coherence {:.2}): masking is \
                 not judged there.",
                same.seconds, same.coherence
            ));
        }
    }
    let mut charts = Vec::new();
    if let Some(f) = crate::rules_same::finding(input, a, b, &same, &mut charts) {
        kept.push(f);
    }
    Ok((kept, charts))
}

/// Every pair compared (in focus, not related, roles allowing), on a few
/// threads ([`crate::map_parallel`]): each profile's band sums decoded once
/// for all its pairs, then the pairs; findings and charts in the pairs'
/// order whatever the threads did.
fn masking(
    input: &DiagnoseInput<'_>,
    out: &mut Vec<Finding>,
    charts: &mut Vec<(u64, crate::details::Chart)>,
) -> Result<(), Cancelled> {
    let tracks: Vec<&TrackInput<'_>> = input.tracks.iter().filter(|t| t.compare).collect();
    let mut pairs = Vec::new();
    for i in 0..tracks.len() {
        for j in i + 1..tracks.len() {
            let (a, b) = (tracks[i], tracks[j]);
            if !(a.focus || b.focus) {
                continue;
            }
            if input
                .related
                .iter()
                .any(|&(x, y)| (x, y) == (a.track, b.track) || (y, x) == (a.track, b.track))
            {
                continue;
            }
            if pair_weight(a.role, b.role).is_none() && pair_weight(b.role, a.role).is_none() {
                continue;
            }
            pairs.push((i, j));
        }
    }
    let mut needed: Vec<usize> = pairs.iter().flat_map(|&(i, j)| [i, j]).collect();
    needed.sort_unstable();
    needed.dedup();
    let decoded = crate::map_parallel(&needed, |&k| crate::same::BandSums::of(tracks[k].profile));
    let mut sums: Vec<Option<crate::same::BandSums>> = vec![None; tracks.len()];
    for (k, d) in needed.into_iter().zip(decoded) {
        sums[k] = Some(d);
    }
    let found = crate::map_parallel(&pairs, |&(i, j)| {
        if input.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Cancelled);
        }
        match (&sums[i], &sums[j]) {
            (Some(x), Some(y)) => pair_masking(input, tracks[i], tracks[j], (x, y)),
            _ => Ok((Vec::new(), Vec::new())),
        }
    });
    for f in found {
        let (findings, made) = f?;
        out.extend(findings);
        charts.extend(made);
    }
    Ok(())
}

fn track_subject(track: Option<TrackId>, master: bool) -> Subject {
    match (track, master) {
        (t, true) => Subject::Master(t),
        (Some(t), false) => Subject::Track(t),
        (None, false) => Subject::Master(None),
    }
}

fn level_finding(
    rule: RuleId,
    signal: SignalPoint,
    subject: Subject,
    severity: f64,
    confidence: f64,
    evidence: Vec<Evidence>,
    recommendations: Vec<Recommendation>,
) -> Finding {
    let key = match subject {
        Subject::Track(t) | Subject::Master(Some(t)) => t.0,
        Subject::Master(None) => u64::MAX,
        Subject::Pair { target, .. } => target.0,
        Subject::Tracks { a, .. } => a.0,
    };
    let tracks = match subject {
        Subject::Track(t) | Subject::Master(Some(t)) => vec![t],
        _ => Vec::new(),
    };
    Finding {
        id: id_of(rule, signal, &[key]),
        rule,
        signal,
        subject,
        tracks,
        time: None,
        events: Vec::new(),
        pattern: None,
        frequency: None,
        severity: severity.clamp(0.0, 1.0),
        severity_terms: Vec::new(),
        confidence: confidence.clamp(0.0, 1.0),
        confidence_terms: Vec::new(),
        evidence,
        spectrum: None,
        sections: Vec::new(),
        recommendations,
        caveats: Vec::new(),
    }
}

fn levels(input: &DiagnoseInput<'_>, out: &mut Vec<Finding>) {
    let rules = &input.rules.levels;
    let sig = input.signal;
    let conf = match sig {
        SignalPoint::Processed => 0.95,
        SignalPoint::Source => 0.6,
    };
    let mut subjects: Vec<(Subject, &TrackProfile, bool)> = input
        .tracks
        .iter()
        .filter(|t| t.focus)
        .map(|t| (Subject::Track(t.track), t.profile, false))
        .collect();
    if let Some((track, p)) = input.master {
        subjects.push((track_subject(track, true), p, true));
    }
    for (subject, p, is_master) in subjects {
        let l = &p.levels;
        let track = match subject {
            Subject::Track(t) | Subject::Master(Some(t)) => Some(t),
            _ => None,
        };
        if p.non_finite > 0 {
            out.push(level_finding(
                RuleId::NonFiniteSamples,
                sig,
                subject,
                0.9,
                0.95,
                vec![Evidence::new(
                    labels::NON_FINITE,
                    p.non_finite as f64,
                    Unit::Count,
                    track,
                )],
                vec![Recommendation {
                    experiment: Experiment::Manual {
                        text: "bypass the track's devices one at a time to find the one \
                               producing invalid samples"
                            .into(),
                    },
                    purpose: "Find the device that fails.".into(),
                    side_effects: "Nothing is changed.".into(),
                }],
            ));
        }
        if is_master && sig == SignalPoint::Processed {
            if l.true_peak.is_finite() && l.true_peak > rules.master_true_peak_dbtp {
                let over = l.true_peak - rules.master_true_peak_dbtp;
                let change = -(round_to(over + 0.25, 0.5)).max(0.5);
                let mut recs = Vec::new();
                if let Some(t) = track
                    && change >= -crate::finding::MAX_LEVEL_DB
                {
                    recs.push(Recommendation {
                        experiment: Experiment::Level {
                            track: t,
                            change_db: change,
                        },
                        purpose: format!(
                            "Bring the true peak under {:+.1} dBTP by lowering the master \
                             fader (after the master's devices)",
                            rules.master_true_peak_dbtp
                        ),
                        side_effects: "The master gets quieter by the same amount; a \
                                       true-peak limiter keeps the loudness instead."
                            .into(),
                    });
                }
                out.push(level_finding(
                    RuleId::MasterTruePeak,
                    sig,
                    subject,
                    0.3 + 0.2 * over,
                    conf,
                    vec![
                        Evidence::new(labels::TRUE_PEAK, l.true_peak, Unit::Dbtp, track),
                        Evidence::new(labels::SAMPLE_PEAK, l.sample_peak, Unit::Dbfs, track),
                        Evidence::new(
                            labels::CEILING,
                            rules.master_true_peak_dbtp,
                            Unit::Dbtp,
                            None,
                        ),
                    ],
                    recs,
                ));
            }
            if l.overs > 0 {
                out.push(level_finding(
                    RuleId::ClippedSamples,
                    sig,
                    subject,
                    0.3 + 0.1 * (1.0 + l.overs as f64).log2(),
                    conf,
                    vec![
                        Evidence::new(labels::OVERS, l.overs as f64, Unit::Count, track),
                        Evidence::new(
                            labels::FULL_SCALE,
                            l.full_scale_samples as f64,
                            Unit::Count,
                            track,
                        ),
                        Evidence::new(labels::SAMPLE_PEAK, l.sample_peak, Unit::Dbfs, track),
                    ],
                    Vec::new(),
                ));
            }
        }
        if !is_master && sig == SignalPoint::Processed && l.above_full_scale > 0 {
            let over = l.sample_peak.max(0.0);
            let mut f = level_finding(
                RuleId::OverFullScale,
                sig,
                subject,
                0.25 + 0.1 * over,
                conf,
                vec![
                    Evidence::new(labels::SAMPLE_PEAK, l.sample_peak, Unit::Dbfs, track),
                    Evidence::new(labels::ABOVE, l.above_full_scale as f64, Unit::Count, track),
                ],
                Vec::new(),
            );
            if let Some(t) = track {
                let change = -(round_to(over + 0.25, 0.5)).max(0.5);
                if change >= -crate::finding::MAX_LEVEL_DB {
                    f.recommendations.push(Recommendation {
                        experiment: Experiment::Level {
                            track: t,
                            change_db: change,
                        },
                        purpose: "Keep the track's own contribution under full scale.".into(),
                        side_effects: "Changes its balance against the other tracks.".into(),
                    });
                }
            }
            out.push(f);
        }
        if is_master && !input.sections.is_empty() {
            let named: Vec<(&str, f64)> = input
                .sections
                .iter()
                .zip(&p.span_loudness)
                .filter(|((_, s), _)| s.seconds() >= rules.min_section_seconds)
                .filter_map(|((n, _), l)| l.map(|l| (n.as_str(), l)))
                .collect();
            for w in named.windows(2) {
                let step = w[1].1 - w[0].1;
                if step.abs() >= rules.section_step_lu {
                    let mut f = level_finding(
                        RuleId::SectionLoudnessStep,
                        sig,
                        subject,
                        0.2,
                        0.9 * conf,
                        vec![
                            Evidence::new(
                                format!("{} {}", labels::INTEGRATED, w[0].0),
                                w[0].1,
                                Unit::Lufs,
                                None,
                            ),
                            Evidence::new(
                                format!("{} {}", labels::INTEGRATED, w[1].0),
                                w[1].1,
                                Unit::Lufs,
                                None,
                            ),
                            Evidence::new(labels::STEP, step, Unit::Lu, None),
                        ],
                        Vec::new(),
                    );
                    f.id = id_of(
                        RuleId::SectionLoudnessStep,
                        sig,
                        &[crate::Fnv::of_str(w[0].0), crate::Fnv::of_str(w[1].0)],
                    );
                    f.sections = vec![w[0].0.to_string(), w[1].0.to_string()];
                    if let Some((_, a)) = input.sections.iter().find(|(n, _)| n == w[0].0)
                        && let Some((_, b)) = input.sections.iter().find(|(n, _)| n == w[1].0)
                    {
                        f.time = Some(TimeSpan::new(a.start, b.end));
                    }
                    out.push(f);
                }
            }
        }
    }
}

fn stereo_rules(input: &DiagnoseInput<'_>, out: &mut Vec<Finding>) {
    let rules = &input.rules.stereo;
    let sig = input.signal;
    let conf = match sig {
        SignalPoint::Processed => 0.9,
        SignalPoint::Source => 0.7,
    };
    let mut subjects: Vec<(Subject, &TrackProfile)> = input
        .tracks
        .iter()
        .filter(|t| t.focus)
        .map(|t| (Subject::Track(t.track), t.profile))
        .collect();
    if let Some((track, p)) = input.master {
        subjects.push((track_subject(track, true), p));
    }
    for (subject, p) in subjects {
        if p.channels < 2 {
            continue;
        }
        let track = match subject {
            Subject::Track(t) | Subject::Master(Some(t)) => Some(t),
            _ => None,
        };
        let hop = p.hop_seconds();
        let longterm = stereo::bands(p, None);
        let top = longterm
            .iter()
            .map(|b| power_db(b.power))
            .fold(f64::NEG_INFINITY, f64::max);
        let threshold = |b: usize| {
            let centre = p.layout.band(b).map_or(0.0, |x| x.centre);
            if centre < rules.low_end_hz {
                rules.fold_loss_db / 2.0
            } else {
                rules.fold_loss_db
            }
        };
        // Per band: seconds the band is relevant and losing.
        let losing: Vec<bool> = (0..p.frames.bands())
            .map(|b| {
                let lvl = power_db(longterm[b].power);
                if lvl < top - rules.relevance_db || lvl < -60.0 {
                    return false;
                }
                let thr = threshold(b);
                let n = (0..p.frames.frames())
                    .filter(|&t| {
                        p.frames.level(t, b) >= top - rules.relevance_db
                            && stereo::BandStereo::of_frame(p, t, b).fold_db() <= -thr
                    })
                    .count();
                n as f64 * hop >= rules.min_seconds && longterm[b].fold_db() <= -thr
            })
            .collect();
        let mut ranges = Vec::new();
        let mut b = 0;
        while b < losing.len() {
            if !losing[b] {
                b += 1;
                continue;
            }
            let a = b;
            while b < losing.len() && losing[b] {
                b += 1;
            }
            ranges.push((a, b - 1));
        }
        // The worst two.
        let loss = |(a, z): (usize, usize)| {
            let (m, c) = (a..=z).fold((0.0, 0.0), |acc, i| {
                (acc.0 + longterm[i].mid, acc.1 + longterm[i].coherent)
            });
            stereo::BandStereo {
                mid: m,
                side: 0.0,
                coherent: c,
                power: 0.0,
                correlation: 0.0,
            }
            .fold_db()
        };
        ranges.sort_by(|x, y| loss(*x).total_cmp(&loss(*y)).then(x.cmp(y)));
        for (a, z) in ranges.into_iter().take(2) {
            let fold = loss((a, z));
            let (lo, hi) = p.layout.span(a, z).unwrap_or((0.0, 0.0));
            let low_end = hi <= rules.low_end_hz * 1.5;
            let severity = (-fold / 12.0).clamp(0.15, 0.9) * if low_end { 1.0 } else { 0.7 };
            let corr = {
                let (mut cw, mut w) = (0.0, 0.0);
                for x in &longterm[a..=z] {
                    cw += x.correlation * x.power;
                    w += x.power;
                }
                if w > 0.0 { cw / w } else { 0.0 }
            };
            let power: f64 = longterm[a..=z].iter().map(|x| x.power).sum();
            let mut recs = Vec::new();
            if let Some(t) = track
                && low_end
            {
                recs.push(Recommendation {
                    experiment: Experiment::SideLowCut {
                        track: t,
                        freq_hz: round_to(hi.clamp(60.0, 250.0), 5.0),
                    },
                    purpose: "Make the low end mono: cut the side signal below the range's top \
                              (the EQ's band on Side), so a mono playback keeps it."
                        .into(),
                    side_effects: "Narrows the stereo image under that frequency; a wide low \
                                   end that was intended goes."
                        .into(),
                });
            }
            recs.push(Recommendation {
                experiment: Experiment::Manual {
                    text: "listen with the master's MONO button and look for the stereo \
                           widener, chorus or out-of-phase microphone causing it"
                        .into(),
                },
                purpose: "Find what makes left and right cancel.".into(),
                side_effects: "Nothing is changed.".into(),
            });
            let mut f = level_finding(
                RuleId::MonoFoldDownLoss,
                sig,
                subject,
                severity,
                conf,
                vec![
                    Evidence::new(labels::FOLD_LOSS, fold, Unit::Db, track),
                    Evidence::new(labels::CORRELATION, corr, Unit::Correlation, track),
                    Evidence::new(labels::BAND_LEVEL, power_db(power), Unit::DbBand, track),
                ],
                recs,
            );
            f.frequency = Some(FrequencyRange { lo, hi });
            f.id = id_of(
                RuleId::MonoFoldDownLoss,
                sig,
                &[track.map_or(u64::MAX, |t| t.0), a as u64],
            );
            out.push(f);
        }
        // Inverted channels: broadband, where both channels carry it alike.
        let (mut cw, mut w, mut n) = (0.0, 0.0, 0usize);
        for t in 0..p.frames.frames() {
            let (c, l, r) = stereo::frame_correlation(p, t);
            let (dl, dr) = (power_db(l), power_db(r));
            if dl < -60.0 || dr < -60.0 || (dl - dr).abs() > 6.0 {
                continue;
            }
            cw += c * (l + r);
            w += l + r;
            n += 1;
        }
        let corr = if w > 0.0 { cw / w } else { 0.0 };
        if n as f64 * hop >= rules.min_seconds && corr <= rules.inverted_correlation {
            out.push(level_finding(
                RuleId::InvertedChannel,
                sig,
                subject,
                0.8,
                conf,
                vec![
                    Evidence::new(labels::CORRELATION, corr, Unit::Correlation, track),
                    Evidence::new(
                        labels::CONFLICT_SECONDS,
                        n as f64 * hop,
                        Unit::Seconds,
                        None,
                    ),
                ],
                vec![Recommendation {
                    experiment: Experiment::Manual {
                        text: "invert one channel's polarity (a utility device) and compare in \
                               mono"
                            .into(),
                    },
                    purpose: "Confirm whether a channel is inverted.".into(),
                    side_effects: "Nothing is changed.".into(),
                }],
            ));
        }
    }
}

fn summary(subject: Subject, p: &TrackProfile, sections: &[(String, TimeSpan)]) -> Summary {
    let l = &p.levels;
    let finite = |v: f64| v.is_finite().then_some(v);
    let mean = p.mean_powers(None);
    let broad = BandLayout::broad()
        .bands()
        .iter()
        .map(|region| {
            let power: f64 = p
                .layout
                .bands()
                .iter()
                .zip(&mean)
                .filter(|(b, _)| b.centre >= region.lo && b.centre < region.hi)
                .map(|(_, v)| *v)
                .sum();
            power_db(power)
        })
        .collect();
    Summary {
        subject,
        sample_peak: l.sample_peak,
        true_peak: l.true_peak,
        rms: l.rms,
        crest: if l.sample_peak.is_finite() && l.rms.is_finite() {
            l.sample_peak - l.rms
        } else {
            0.0
        },
        integrated: l.loudness.and_then(|x| finite(x.integrated)),
        loudness_range: l
            .loudness
            .and_then(|x| finite(x.range))
            .filter(|_| p.seconds() >= 3.0),
        max_short_term: l.loudness.and_then(|x| finite(x.max_short_term)),
        correlation: stereo::overall_correlation(p),
        broad,
        sections: sections
            .iter()
            .zip(p.span_loudness.iter().chain(std::iter::repeat(&None)))
            .map(|((n, _), l)| (n.clone(), *l))
            .collect(),
    }
}

/// Run every rule (see the module docs): findings most relevant first
/// (ties by id), and a summary per subject.
pub fn diagnose(input: &DiagnoseInput<'_>) -> Result<Report, Cancelled> {
    let mut findings = Vec::new();
    let mut details = crate::details::Details::default();
    masking(input, &mut findings, &mut details.charts)?;
    if input.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(Cancelled);
    }
    levels(input, &mut findings);
    stereo_rules(input, &mut findings);
    crate::rules_width::run(input, &mut findings, &mut details);
    crate::rules_dynamics::run(input, &mut findings, &mut details);
    findings.retain(|f| f.severity.is_finite() && f.confidence.is_finite());
    rank(&mut findings);
    let mut summaries: Vec<Summary> = input
        .tracks
        .iter()
        .map(|t| summary(Subject::Track(t.track), t.profile, &[]))
        .collect();
    if let Some((track, p)) = input.master {
        summaries.push(summary(track_subject(track, true), p, &input.sections));
    }
    Ok(Report {
        findings,
        summaries,
        details,
    })
}

/// Findings of a category.
pub fn of_category(findings: &[Finding], c: Category) -> impl Iterator<Item = &Finding> {
    findings.iter().filter(move |f| f.category() == c)
}
