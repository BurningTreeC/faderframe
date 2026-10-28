//! The phase and timing rules, on pairs measured by [`crate::phase`], and
//! the diagnosis of what needs more than one profile per track
//! ([`diagnose_extra`]: those pairs, and in a processed analysis each
//! track's source for [`RuleId::TransientFlattening`]). Thresholds in
//! [`crate::config::PhaseRule`] and [`crate::config::TimingRule`].
//!
//! Over the bands both tracks are *relevant* in (within `relevance_db` of
//! their own loudest band, above `floor_db`), the *broadband* sum against
//! the energies is `Σ (P_a + P_b + 2·cross) / Σ (P_a + P_b)` in dB.
//!
//! * [`RuleId::PolarityInverted`] — the delay estimator related and sure
//!   (`min_confidence`) and its peak negative: the correction is the
//!   polarity, with the delay when it lies between `min_offset_ms` and
//!   `max_comb_ms` and the peak is clear (`min_clarity`); reported when it
//!   brings back `min_gain_db` or more.
//! * [`RuleId::TimeOffset`] — related, sure, clear, positive, a delay
//!   between `min_offset_ms` and `max_comb_ms`; reported when correcting
//!   it brings back `min_gain_db` broadband, or lifts the most cancelled
//!   band (at `cancel_db` or worse) by 3 dB.
//! * [`RuleId::FrequencyDependentPhase`] — relevant bands coherent
//!   (`min_coherence`) whose sum is `cancel_db` or more under the energies
//!   *after* the correction above (or as they are, without one):
//!   contiguous ones make a range, at most two (the worst first).
//! * [`RuleId::OnsetOffset`] / [`RuleId::LooseTiming`] — at least
//!   `min_onsets` onsets paired, `min_share` of the sparser track's; an
//!   offset the delay estimator already explains (within 2 ms) is left to
//!   it. Consistent: `|median| ≥ flam_ms` and spread `≤ consistent_ms +
//!   consistent_share · |median|`. Loose: spread `≥ loose_ms` between
//!   parts said to share a source (roles, selection).
//!
//! Severity = pair weight (1 for pairs said to share a source, kick and
//! bass; 0.8 otherwise) × a term of the measured effect:
//!
//! * polarity and offset: `clamp(0.2 + gain / 6, 0.2, 1)`;
//! * frequency-dependent: `clamp(0.15 + loss / 12, 0.15, 1)` × importance
//!   (`clamp(share / 0.25, 0.25, 1)` of the pair's energy in the range);
//! * onsets: `clamp(0.15 + (|median| − flam) / 40, 0.15, 0.5)`;
//! * loose: `clamp(0.15 + (spread − loose) / 40, 0.15, 0.4)`.
//!
//! Confidence = signal (processed 1, source 0.75) × amount (`0.6 + 0.4 ·
//! min(1, together / 8 s)`) × the estimator's: its confidence × `(0.6 +
//! 0.4 · clarity)`, the range's mean coherence, or the share of onsets
//! paired × `(0.6 + 0.4 · min(1, paired / 16))`.
//!
//! Which track an experiment moves: the one that is less of a reference
//! by role (a lead keeps its place, then the kit's "Drums" track, the
//! kick, the snare, others; a bass moves first), else the later one is
//! moved earlier (Auto-Align's convention). Never more than
//! [`crate::finding::MAX_ALIGN_MS`].

use crate::config::PhaseRule;
use crate::details::{Chart, ChartSeries, Details, PairSummary};
use crate::finding::{
    Evidence, Experiment, Finding, FrequencyRange, Recommendation, RuleId, SignalPoint, Subject,
    TimeSpan, Unit, labels,
};
use crate::phase::{BandPhase, PairCandidate, PairPhase, PairReason};
use crate::profile::{TrackProfile, power_db};
use crate::rulekit::{blank, merge_spans, round_to, score, span_of, term};
use crate::rules::{Cancelled, DiagnoseInput, Report};
use faderframe_core::{TrackId, TrackRole};
use std::sync::atomic::Ordering;

/// A measured pair.
pub struct PairInput<'a> {
    pub candidate: &'a PairCandidate,
    pub phase: &'a PairPhase,
}

/// A track's source profile (processed analyses: what its processing is
/// compared against).
pub struct SourceInput<'a> {
    pub track: TrackId,
    pub profile: &'a TrackProfile,
    /// The track feeds sends (their returns are in its processed signal).
    pub sends: bool,
}

/// What [`diagnose_extra`] reads beside [`DiagnoseInput`].
#[derive(Default)]
pub struct ExtraInput<'a> {
    pub pairs: Vec<PairInput<'a>>,
    pub sources: Vec<SourceInput<'a>>,
    /// The master or a bus has devices (processed renders of one track go
    /// through them).
    pub master_devices: bool,
    /// Candidates not measured, and why.
    pub skipped: Vec<(TrackId, TrackId, String)>,
}

/// How a suggested correction changes the pair.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Correction {
    /// The measured delay and polarity taken out.
    Aligned,
    /// Only the polarity flipped.
    Flipped,
    /// The median onset offset taken out.
    Onsets,
}

fn after(b: &BandPhase, c: Option<Correction>) -> Option<f64> {
    match c {
        Some(Correction::Aligned) => b.aligned_db(),
        Some(Correction::Flipped) => b.flipped_db(),
        Some(Correction::Onsets) => b.onsets_aligned_db(),
        None => b.sum_db(),
    }
}

/// The cross term after a correction.
fn cross_after(b: &BandPhase, c: Option<Correction>) -> Option<f64> {
    match c {
        Some(Correction::Aligned) => b.cross_aligned,
        Some(Correction::Flipped) => Some(-b.cross),
        Some(Correction::Onsets) => b.cross_onsets,
        None => Some(b.cross),
    }
}

fn relevant(ph: &PairPhase, rule: &PhaseRule) -> Vec<bool> {
    let top = |f: &dyn Fn(&BandPhase) -> f64| {
        ph.bands
            .iter()
            .map(|b| power_db(f(b)))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let (ta, tb) = (top(&|b| b.power_a), top(&|b| b.power_b));
    ph.bands
        .iter()
        .map(|b| {
            power_db(b.power_a) >= (ta - rule.relevance_db).max(rule.floor_db)
                && power_db(b.power_b) >= (tb - rule.relevance_db).max(rule.floor_db)
        })
        .collect()
}

/// The sum against the energies over `bands` where `on` (dB), with the
/// cross term `cross`.
fn over(
    bands: &[BandPhase],
    on: &dyn Fn(usize) -> bool,
    cross: &dyn Fn(&BandPhase) -> Option<f64>,
    mono: bool,
) -> Option<f64> {
    let (mut s, mut e) = (0.0, 0.0);
    for (_, b) in bands.iter().enumerate().filter(|(i, _)| on(*i)) {
        let p = if mono {
            b.mono_a + b.mono_b
        } else {
            b.power_a + b.power_b
        };
        s += p + 2.0 * cross(b)?;
        e += p;
    }
    (e > 0.0).then(|| power_db(s.max(0.0)) - power_db(e))
}

fn stays(role: Option<TrackRole>) -> u8 {
    match role {
        Some(TrackRole::LeadVocal | TrackRole::LeadInstrument) => 0,
        Some(TrackRole::Drums) => 1,
        Some(TrackRole::Kick) => 2,
        Some(TrackRole::Snare) => 3,
        Some(TrackRole::Bass) => 5,
        _ => 4,
    }
}

/// Which track an alignment moves and by how much (ms) to undo `lag_ms`
/// (positive: `b` later).
fn mover(input: &DiagnoseInput<'_>, c: &PairCandidate, lag_ms: f64) -> (TrackId, f64) {
    let role = |t: TrackId| {
        input
            .tracks
            .iter()
            .find(|x| x.track == t)
            .and_then(|x| x.role)
    };
    let (ra, rb) = (stays(role(c.a)), stays(role(c.b)));
    let move_b = if ra != rb { ra < rb } else { lag_ms >= 0.0 };
    // (+ 0.0: never a negative zero.)
    let change = |v: f64| round_to(v, 1e-4) + 0.0;
    if move_b {
        (c.b, change(-lag_ms))
    } else {
        (c.a, change(lag_ms))
    }
}

fn weight(c: &PairCandidate) -> f64 {
    if c.same_source() || c.reasons.contains(&PairReason::KickAndBass) {
        1.0
    } else {
        0.8
    }
}

fn signal_conf(s: SignalPoint) -> f64 {
    match s {
        SignalPoint::Processed => 1.0,
        SignalPoint::Source => 0.75,
    }
}

fn pair_finding(
    rule: RuleId,
    input: &DiagnoseInput<'_>,
    c: &PairCandidate,
    ph: &PairPhase,
    part: u64,
) -> Finding {
    let mut f = blank(
        rule,
        input.signal,
        Subject::Tracks { a: c.a, b: c.b },
        &[c.a.0, c.b.0, part],
    );
    f.time = Some(ph.excerpt);
    f.events = vec![ph.excerpt];
    let why: Vec<&str> = c.reasons.iter().map(|r| r.label()).collect();
    f.caveats.push(format!(
        "Compared because {}; over an excerpt of {:.1} s where both play.",
        why.join(", "),
        ph.excerpt.seconds()
    ));
    if input.signal == SignalPoint::Source {
        f.caveats.push(
            "Measured before processing: devices with latency or phase shifts, the track \
             delays and polarities in the mix are not part of it."
                .into(),
        );
    }
    f
}

fn sum_chart(
    ph: &PairPhase,
    rel: &[bool],
    correction: Option<Correction>,
    highlight: Option<(usize, usize)>,
) -> Chart {
    let pick = |f: &dyn Fn(&BandPhase) -> Option<f64>| -> Vec<f64> {
        ph.bands
            .iter()
            .zip(rel)
            .map(|(b, r)| {
                if *r {
                    f(b).unwrap_or(f64::NAN)
                } else {
                    f64::NAN
                }
            })
            .collect()
    };
    let mut series = vec![ChartSeries::new("Sum now", None, pick(&BandPhase::sum_db))];
    match correction {
        Some(Correction::Aligned) => series.push(ChartSeries::new(
            "Corrected",
            None,
            pick(&BandPhase::aligned_db),
        )),
        Some(Correction::Onsets) => series.push(ChartSeries::new(
            "Onsets aligned",
            None,
            pick(&BandPhase::onsets_aligned_db),
        )),
        Some(Correction::Flipped) | None => series.push(ChartSeries::new(
            "Polarity flipped",
            None,
            pick(&BandPhase::flipped_db),
        )),
    }
    series.push(ChartSeries::new("In mono", None, pick(&BandPhase::mono_db)));
    Chart::Bands {
        centres: ph.bands.iter().map(|b| b.centre).collect(),
        series,
        highlight,
        unit: Unit::Db,
        zero: true,
        caption: "the two tracks' sum against the sum of their energies (dB): 0 unrelated, \
                  +3 equal and in phase, below 0 cancelling"
            .into(),
    }
}

fn worst_band(ph: &PairPhase, rel: &[bool]) -> Option<(usize, f64)> {
    ph.bands
        .iter()
        .enumerate()
        .filter(|(i, _)| rel[*i])
        .filter_map(|(i, b)| b.sum_db().map(|s| (i, s)))
        .min_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)))
}

#[allow(clippy::too_many_arguments)]
fn lag_evidence(
    f: &mut Finding,
    c: &PairCandidate,
    ph: &PairPhase,
    l: &crate::phase::LagEstimate,
    now: f64,
    after: Option<f64>,
    mono: Option<f64>,
) {
    let fr = f64::from(ph.rate.max(1));
    f.evidence.extend([
        Evidence::new(labels::LAG, l.ms, Unit::Ms, Some(c.b)),
        Evidence::new(
            labels::LAG_SAMPLES,
            l.ms / 1000.0 * fr,
            Unit::Samples,
            Some(c.b),
        ),
        Evidence::new(labels::LAG_CONFIDENCE, l.confidence, Unit::Ratio, None),
        Evidence::new(labels::CLARITY, l.clarity, Unit::Ratio, None),
        Evidence::new(
            labels::POLARITY,
            if l.inverted { -1.0 } else { 1.0 },
            Unit::Correlation,
            None,
        ),
        Evidence::new(labels::SUM_NOW, now, Unit::Db, None),
    ]);
    if let Some(a) = after {
        f.evidence
            .push(Evidence::new(labels::SUM_CORRECTED, a, Unit::Db, None));
        f.evidence
            .push(Evidence::new(labels::GAIN, a - now, Unit::Db, None));
    }
    if let Some(m) = mono {
        f.evidence
            .push(Evidence::new(labels::SUM_MONO, m, Unit::Db, None));
    }
    f.evidence.push(Evidence::new(
        labels::TOGETHER,
        ph.together,
        Unit::Seconds,
        None,
    ));
}

/// A pair's findings and summary.
fn pair(
    input: &DiagnoseInput<'_>,
    p: &PairInput<'_>,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let (c, ph) = (p.candidate, p.phase);
    let rule = &input.rules.phase;
    let rel = relevant(ph, rule);
    let all = |i: usize| rel.get(i) == Some(&true);
    let now = over(&ph.bands, &all, &|b| Some(b.cross), false);
    let corrected = over(&ph.bands, &all, &|b| b.cross_aligned, false);
    let flipped = over(&ph.bands, &all, &|b| Some(-b.cross), false);
    let mono = over(&ph.bands, &all, &|b| Some(b.mono_cross), true);
    let coherence = {
        let (mut s, mut w) = (0.0, 0.0);
        for (b, r) in ph.bands.iter().zip(&rel) {
            if *r {
                let e = b.power_a + b.power_b;
                s += b.coherence * e;
                w += e;
            }
        }
        (w > 0.0).then(|| s / w)
    };
    let amount = 0.6 + 0.4 * (ph.together / 8.0).min(1.0);
    let usable = ph
        .lag
        .filter(|l| l.related && l.confidence >= rule.min_confidence);
    let mut correction = None;
    let mut corrected_lag = None;
    if let (Some(l), Some(now)) = (usable, now) {
        let clear = l.clarity >= rule.min_clarity;
        let in_comb = l.ms.abs() <= rule.max_comb_ms;
        let offset = l.ms.abs() >= rule.min_offset_ms;
        // A polarity is a broadband claim: it needs a clear peak (a
        // periodic or tonal relationship is left to the per-band rule).
        if l.inverted && clear {
            let with_delay = offset && in_comb;
            let (kind, after_v) = if with_delay || !offset {
                (Correction::Aligned, corrected)
            } else {
                (Correction::Flipped, flipped)
            };
            let gain = after_v.map_or(0.0, |a| a - now);
            if gain >= rule.min_gain_db {
                let mut f = pair_finding(RuleId::PolarityInverted, input, c, ph, 0);
                lag_evidence(&mut f, c, ph, &l, now, after_v, mono);
                let (moved, change) = mover(input, c, if with_delay { l.ms } else { 0.0 });
                f.recommendations.push(Recommendation {
                    experiment: Experiment::Alignment {
                        track: moved,
                        change_ms: change,
                        invert: true,
                    },
                    purpose: if change != 0.0 {
                        "Turn the track over and take out the delay found, so the shared signal \
                         adds instead of cancelling."
                            .into()
                    } else {
                        "Turn the track over so the shared signal adds instead of cancelling."
                            .into()
                    },
                    side_effects: "Changes how the two sum everywhere they play together; if the \
                                   inversion was meant (a creative cancellation), it goes."
                        .into(),
                });
                if change != 0.0 {
                    f.recommendations.push(Recommendation {
                        experiment: Experiment::Alignment {
                            track: moved,
                            change_ms: 0.0,
                            invert: true,
                        },
                        purpose: "Only the polarity, keeping the delay (if it is part of the \
                                  sound)."
                            .into(),
                        side_effects: "The comb filtering of the delay stays.".into(),
                    });
                }
                score(
                    &mut f,
                    vec![
                        term("pair weight", weight(c)),
                        term("gain", (0.2 + gain / 6.0).clamp(0.2, 1.0)),
                    ],
                    vec![
                        term("signal", signal_conf(input.signal)),
                        term("estimator", l.confidence * (0.6 + 0.4 * l.clarity)),
                        term("amount", amount),
                    ],
                );
                details.charts.push((
                    f.id,
                    sum_chart(
                        ph,
                        &rel,
                        Some(kind),
                        worst_band(ph, &rel).map(|(i, _)| (i, i)),
                    ),
                ));
                out.push(f);
                correction = Some(kind);
                corrected_lag = Some(l.ms);
            }
        } else if offset && in_comb && clear {
            let gain = corrected.map_or(0.0, |a| a - now);
            let worst = worst_band(ph, &rel);
            let lifted = worst.is_some_and(|(i, s)| {
                s <= -rule.cancel_db && ph.bands[i].aligned_db().is_some_and(|a| a - s >= 3.0)
            });
            if gain >= rule.min_gain_db || lifted {
                let mut f = pair_finding(RuleId::TimeOffset, input, c, ph, 0);
                lag_evidence(&mut f, c, ph, &l, now, corrected, mono);
                f.evidence.extend([
                    Evidence::new(
                        labels::FIRST_NOTCH,
                        1000.0 / (2.0 * l.ms.abs()),
                        Unit::Hz,
                        None,
                    ),
                    Evidence::new(labels::NOTCH_SPACING, 1000.0 / l.ms.abs(), Unit::Hz, None),
                ]);
                if let Some((i, s)) = worst {
                    f.evidence.extend([
                        Evidence::new(labels::WORST_BAND, ph.bands[i].centre, Unit::Hz, None),
                        Evidence::new(labels::WORST_BAND_SUM, s, Unit::Db, None),
                    ]);
                }
                let (moved, change) = mover(input, c, l.ms);
                f.recommendations.push(Recommendation {
                    experiment: Experiment::Alignment {
                        track: moved,
                        change_ms: change,
                        invert: false,
                    },
                    purpose: "Take out the delay between them (to a fraction of a sample) so the \
                              shared signal sums without a comb filter."
                        .into(),
                    side_effects: "Moves the track in time by that much (its timing against \
                                   everything else changes slightly); a distance that gave the \
                                   sound its depth goes."
                        .into(),
                });
                f.recommendations.push(Recommendation {
                    experiment: Experiment::Manual {
                        text: "if the offset is part of the sound (a room microphone, a \
                               doubling), leave it; or try part of the delay by ear in the \
                               track's Delay field"
                            .into(),
                    },
                    purpose: "Some offsets are musical: aligning to the highest correlation is \
                              not always better."
                        .into(),
                    side_effects: "Nothing is changed.".into(),
                });
                score(
                    &mut f,
                    vec![
                        term("pair weight", weight(c)),
                        term("gain", (0.2 + gain.max(0.0) / 6.0).clamp(0.2, 1.0)),
                    ],
                    vec![
                        term("signal", signal_conf(input.signal)),
                        term("estimator", l.confidence * (0.6 + 0.4 * l.clarity)),
                        term("amount", amount),
                    ],
                );
                details.charts.push((
                    f.id,
                    sum_chart(
                        ph,
                        &rel,
                        Some(Correction::Aligned),
                        worst.map(|(i, _)| (i, i)),
                    ),
                ));
                out.push(f);
                correction = Some(Correction::Aligned);
                corrected_lag = Some(l.ms);
            }
        }
    }
    // An onset offset suggested as an experiment corrects the pair too: what
    // it explains is not frequency-dependent.
    if onsets(input, c, ph, &rel, corrected_lag, out, details) && correction.is_none() {
        correction = Some(Correction::Onsets);
    }
    frequency_dependent(input, c, ph, &rel, correction, amount, out, details);
    details.pairs.push(PairSummary {
        a: c.a,
        b: c.b,
        reasons: c.reasons.clone(),
        excerpt: ph.excerpt,
        together: ph.together,
        lag: ph.lag,
        coherence,
        sum_db: now,
        corrected_db: corrected,
        mono_db: mono,
        onsets_paired: ph.onsets.matched.len(),
        onset_median_ms: ph.onsets.median_ms,
        onset_spread_ms: ph.onsets.spread_ms,
    });
}

#[allow(clippy::too_many_arguments)]
fn frequency_dependent(
    input: &DiagnoseInput<'_>,
    c: &PairCandidate,
    ph: &PairPhase,
    rel: &[bool],
    correction: Option<Correction>,
    amount: f64,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let rule = &input.rules.phase;
    let cancelling: Vec<bool> = ph
        .bands
        .iter()
        .zip(rel)
        .map(|(b, r)| {
            *r && b.coherence >= rule.min_coherence
                && after(b, correction).is_some_and(|s| s <= -rule.cancel_db)
        })
        .collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < cancelling.len() {
        if !cancelling[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i < cancelling.len() && cancelling[i] {
            i += 1;
        }
        ranges.push((a, i - 1));
    }
    let cross_after = |b: &BandPhase| cross_after(b, correction);
    let total: f64 = ph
        .bands
        .iter()
        .zip(rel)
        .filter(|(_, r)| **r)
        .map(|(b, _)| b.power_a + b.power_b)
        .sum();
    let mut scored: Vec<((usize, usize), f64)> = ranges
        .into_iter()
        .filter_map(|(a, z)| {
            let s = over(&ph.bands, &|i| (a..=z).contains(&i), &cross_after, false)?;
            (s <= -rule.cancel_db).then_some(((a, z), s))
        })
        .collect();
    scored.sort_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)));
    for ((a, z), s) in scored.into_iter().take(2) {
        let inside = |i: usize| (a..=z).contains(&i);
        let energy: f64 = ph.bands[a..=z].iter().map(|b| b.power_a + b.power_b).sum();
        let share = if total > 0.0 { energy / total } else { 0.0 };
        let coh = {
            let (mut x, mut w) = (0.0, 0.0);
            for b in &ph.bands[a..=z] {
                let e = b.power_a + b.power_b;
                x += b.coherence * e;
                w += e;
            }
            if w > 0.0 { x / w } else { 0.0 }
        };
        // The phase left there (power-weighted circular mean, degrees).
        let phase = {
            let (mut re, mut im) = (0.0, 0.0);
            for b in &ph.bands[a..=z] {
                let e = b.power_a + b.power_b;
                let p = match correction {
                    Some(Correction::Aligned) => b.residual.unwrap_or(b.phase),
                    Some(Correction::Onsets) => b.residual_onsets.unwrap_or(b.phase),
                    Some(Correction::Flipped) => {
                        let q = b.phase + std::f64::consts::PI;
                        q.sin().atan2(q.cos())
                    }
                    None => b.phase,
                };
                re += e * p.cos();
                im += e * p.sin();
            }
            im.atan2(re).to_degrees()
        };
        let flip_range = over(&ph.bands, &inside, &|b| Some(-b.cross), false);
        let flip_all = over(&ph.bands, &|i| rel[i], &|b| Some(-b.cross), false);
        let now_all = over(&ph.bands, &|i| rel[i], &|b| Some(b.cross), false);
        let mono_range = over(&ph.bands, &inside, &|b| Some(b.mono_cross), true);
        let (lo, hi) = (ph.bands[a].lo, ph.bands[z].hi);
        let mut f = pair_finding(RuleId::FrequencyDependentPhase, input, c, ph, a as u64);
        f.frequency = Some(FrequencyRange { lo, hi });
        f.evidence = vec![
            Evidence::new(labels::COHERENCE, coh, Unit::Ratio, None),
            Evidence::new(labels::PHASE, phase, Unit::Degrees, Some(c.b)),
            Evidence::new(
                if correction.is_some() {
                    labels::SUM_CORRECTED
                } else {
                    labels::SUM_NOW
                },
                s,
                Unit::Db,
                None,
            ),
        ];
        if correction.is_some() {
            // The title and explanation read the sum there.
            f.evidence
                .push(Evidence::new(labels::SUM_NOW, s, Unit::Db, None));
            f.caveats.push(
                "This is what remains after the delay or polarity correction suggested for the \
                 pair."
                    .into(),
            );
        }
        if let Some(v) = flip_range {
            f.evidence
                .push(Evidence::new(labels::SUM_FLIPPED, v, Unit::Db, None));
        }
        if let Some(v) = mono_range {
            f.evidence
                .push(Evidence::new(labels::SUM_MONO, v, Unit::Db, None));
        }
        f.evidence.extend([
            Evidence::new(labels::PAIR_SHARE, share, Unit::Share, None),
            Evidence::new(labels::TOGETHER, ph.together, Unit::Seconds, None),
        ]);
        let (moved, _) = mover(input, c, 0.0);
        if correction.is_none()
            && let (Some(fr), Some(fa), Some(na)) = (flip_range, flip_all, now_all)
            && fr - s >= 3.0
            && fa >= na - 0.5
        {
            f.recommendations.push(Recommendation {
                experiment: Experiment::Alignment {
                    track: moved,
                    change_ms: 0.0,
                    invert: true,
                },
                purpose: format!(
                    "Flipping the polarity is measured to bring this range to {fr:+.1} dB (from \
                     {s:+.1} dB) and the whole pair to {fa:+.1} dB (from {na:+.1} dB)."
                ),
                side_effects: "Changes the relationship at every frequency, not only here; \
                               listen to the whole range."
                    .into(),
            });
        }
        let centre = (lo * hi).sqrt();
        f.recommendations.push(Recommendation {
            experiment: Experiment::Manual {
                text: format!(
                    "rotate the phase of one of the two around {} with the built-in EQ's All \
                     Pass band (start at Q 0.7 and sweep the frequency by ear while both play), \
                     or check the high-pass filters and EQs on both tracks (a filter turns the \
                     phase near its corner), or a microphone's position",
                    crate::bands::format_hz(centre)
                ),
            },
            purpose: "Bring their phases together in the range without moving either track \
                      in time; the analysis does not design the all-pass, it only says where."
                .into(),
            side_effects: "An all-pass changes the phase over an octave or two around its \
                           frequency: it can improve one range and worsen another."
                .into(),
        });
        score(
            &mut f,
            vec![
                term("pair weight", weight(c)),
                term("loss", (0.15 + -s / 12.0).clamp(0.15, 1.0)),
                term("importance", (share / 0.25).clamp(0.25, 1.0)),
            ],
            vec![
                term("signal", signal_conf(input.signal)),
                term("coherence", coh.clamp(0.0, 1.0)),
                term("amount", amount),
            ],
        );
        details
            .charts
            .push((f.id, sum_chart(ph, rel, correction, Some((a, z)))));
        out.push(f);
    }
}

/// The onset rules; whether an offset was suggested as an experiment.
fn onsets(
    input: &DiagnoseInput<'_>,
    c: &PairCandidate,
    ph: &PairPhase,
    rel: &[bool],
    corrected_lag: Option<f64>,
    out: &mut Vec<Finding>,
    details: &mut Details,
) -> bool {
    let tim = &input.rules.timing;
    let o = &ph.onsets;
    let n = o.matched.len();
    let (Some(med), Some(spread)) = (o.median_ms, o.spread_ms) else {
        return false;
    };
    if n < tim.min_onsets || o.share() < tim.min_share {
        return false;
    }
    // A delay finding of the estimator's for the same offset speaks for it.
    if corrected_lag.is_some_and(|l| (l - med).abs() <= 2.0) {
        return false;
    }
    let consistent =
        med.abs() >= tim.flam_ms && spread <= tim.consistent_ms + tim.consistent_share * med.abs();
    let loose = !consistent && spread >= tim.loose_ms && c.same_source();
    if !(consistent || loose) {
        return false;
    }
    let mut suggested = false;
    let conf = vec![
        term("signal", signal_conf(input.signal)),
        term("paired", o.share().min(1.0)),
        term("amount", 0.6 + 0.4 * (n as f64 / 16.0).min(1.0)),
    ];
    let (rule_id, events) = if consistent {
        (
            RuleId::OnsetOffset,
            o.matched
                .iter()
                .map(|(t, _)| TimeSpan::new((t - 0.05).max(0.0), t + 0.15))
                .collect::<Vec<_>>(),
        )
    } else {
        (
            RuleId::LooseTiming,
            o.matched
                .iter()
                .filter(|(_, d)| (d - med).abs() >= tim.loose_ms)
                .map(|(t, _)| TimeSpan::new((t - 0.05).max(0.0), t + 0.15))
                .collect(),
        )
    };
    let mut f = pair_finding(rule_id, input, c, ph, 0);
    f.events = merge_spans(events, 0.3);
    f.time = span_of(&f.events).or(Some(ph.excerpt));
    if f.events.is_empty() {
        f.events = vec![ph.excerpt];
    }
    f.evidence = vec![
        Evidence::new(labels::ONSET_MEDIAN, med, Unit::Ms, Some(c.b)),
        Evidence::new(labels::ONSET_SPREAD, spread, Unit::Ms, None),
        Evidence::new(labels::ONSETS_MATCHED, n as f64, Unit::Count, None),
        Evidence::new(labels::ONSET_SHARE, o.share(), Unit::Share, None),
    ];
    if consistent {
        // What the pair's sum would be with the offset taken out.
        let all = |i: usize| rel.get(i) == Some(&true);
        let now = over(&ph.bands, &all, &|b| Some(b.cross), false);
        let aligned = over(&ph.bands, &all, &|b| b.cross_onsets, false);
        if let (Some(x), Some(y)) = (now, aligned) {
            f.evidence.extend([
                Evidence::new(labels::SUM_NOW, x, Unit::Db, None),
                Evidence::new(labels::SUM_CORRECTED, y, Unit::Db, None),
            ]);
        }
        if med.abs() <= tim.max_shift_ms {
            suggested = true;
            let (moved, change) = mover(input, c, med);
            f.recommendations.push(Recommendation {
                experiment: Experiment::Alignment {
                    track: moved,
                    change_ms: change,
                    invert: false,
                },
                purpose: "Hear the two with their note starts together.".into(),
                side_effects: "Moves every note of the track by the same amount, also where the \
                               offset is not heard as a flam; the feel changes."
                    .into(),
            });
        } else {
            f.caveats.push(format!(
                "The offset is larger than {:.0} ms: moving the whole track is not suggested \
                 (it would change the part, not its alignment).",
                tim.max_shift_ms
            ));
        }
        f.recommendations.push(Recommendation {
            experiment: Experiment::Manual {
                text: "if the feel is intended, leave it; to tighten single notes rather than \
                       the whole track, edit them (warp markers on transients, Quantize by \
                       transients)"
                    .into(),
            },
            purpose: "Timing is often the groove: decide by listening.".into(),
            side_effects: "Nothing is changed.".into(),
        });
        score(
            &mut f,
            vec![
                term("pair weight", weight(c)),
                term(
                    "offset",
                    (0.15 + (med.abs() - tim.flam_ms) / 40.0).clamp(0.15, 0.5),
                ),
            ],
            conf,
        );
    } else {
        f.recommendations.push(Recommendation {
            experiment: Experiment::Manual {
                text: "tighten the double by hand (warp markers on its transients, Quantize by \
                       transients to the other part) or keep it loose for width"
                    .into(),
            },
            purpose: "Decide whether the double should be tighter.".into(),
            side_effects: "Nothing is changed.".into(),
        });
        score(
            &mut f,
            vec![
                term("pair weight", weight(c)),
                term(
                    "spread",
                    (0.15 + (spread - tim.loose_ms) / 40.0).clamp(0.15, 0.4),
                ),
            ],
            conf,
        );
    }
    details.charts.push((
        f.id,
        Chart::Offsets {
            values: o.matched.iter().map(|m| m.1).collect(),
            median: Some(med),
            caption: "onset offsets of the second track against the first (ms)".into(),
        },
    ));
    out.push(f);
    suggested
}

/// Run the rules that read more than one profile per track (see the
/// module docs), adding their findings to `report` (ranked again) and
/// their measurements to its details.
pub fn diagnose_extra(
    input: &DiagnoseInput<'_>,
    extra: &ExtraInput<'_>,
    report: &mut Report,
) -> Result<(), Cancelled> {
    let mut found = Vec::new();
    for p in &extra.pairs {
        if input.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Cancelled);
        }
        pair(input, p, &mut found, &mut report.details);
    }
    for s in &extra.sources {
        let Some(t) = input.tracks.iter().find(|t| t.track == s.track && t.focus) else {
            continue;
        };
        crate::rules_dynamics::flattening(
            input,
            s.track,
            t.role,
            t.profile,
            s.profile,
            s.sends,
            extra.master_devices,
            &mut found,
            &mut report.details,
        );
    }
    report
        .details
        .skipped_pairs
        .extend(extra.skipped.iter().cloned());
    report.add(found);
    Ok(())
}
