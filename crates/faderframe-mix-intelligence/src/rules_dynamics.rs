//! The dynamics rules. Thresholds in [`crate::config::DynamicsRule`];
//! indicators defined in [`crate::dynamics`].
//!
//! * [`RuleId::WideLoudnessRange`] — a focus track whose LRA is at least
//!   `track_lra_lu`, wider than the mix's by `lra_margin_lu` (when the mix
//!   was measured), over `min_active_seconds` of gated short-term
//!   loudness. Severity `clamp(0.15 + 0.03 · (LRA − threshold), 0.15,
//!   0.5)`; confidence signal (processed 0.8, source 0.6) × amount (`0.6 +
//!   0.4 · min(1, active / 60 s)`) × context (1 with the mix measured,
//!   0.8 without).
//! * [`RuleId::LevelJump`] — a focus track's momentary loudness changing
//!   by `step_lu` or more between the 2 s before and after a moment, both
//!   sides above its gate (−70 LUFS, 20 LU under its integrated
//!   loudness), when the mix changes by less than `mirrored_share` of it
//!   there. Severity `clamp(0.2 + 0.02 · (|step| − threshold), 0.2, 0.5)`;
//!   confidence signal × context.
//! * [`RuleId::TransientFlattening`] — processed analyses, from the
//!   track's source profile ([`flattening`]): the source's median
//!   transient-to-sustain at least `min_source_tsr_db` and the processed
//!   signal's lower by `flatten_db` or more at the same events. Severity
//!   role (drums, percussion 1; others 0.8) × `clamp(0.2 + (drop −
//!   threshold) / 10, 0.2, 0.7)`; confidence 0.85 × amount (`0.6 + 0.4 ·
//!   min(1, events / 24)`) × sends (0.7 when the track feeds sends, whose
//!   returns add sustain) × agreement (1 when the crest factor fell by 1 dB
//!   or more as well, else 0.85). Worded "possible", never naming a
//!   device or its settings.

use crate::details::{Chart, ChartSeries, Details, DynamicsSummary};
use crate::dynamics::{self, DynamicsTrace, LOUDNESS_STEP, MOMENTARY_LAG, SHORT_TERM_LAG};
use crate::finding::{
    Evidence, Experiment, Finding, Recommendation, RuleId, SignalPoint, Subject, TimeSpan, Unit,
    labels,
};
use crate::profile::TrackProfile;
use crate::rulekit::{blank, score, span_of, term};
use crate::rules::DiagnoseInput;
use faderframe_core::{RoleFamily, TrackId, TrackRole};

fn nan_inf(v: f64) -> f64 {
    if v.is_finite() { v } else { f64::NAN }
}

/// A chart of loudness over time: the track's and the mix's.
fn loudness_chart(
    track: Option<TrackId>,
    p: &TrackProfile,
    master: Option<&TrackProfile>,
    short_term: bool,
    highlight: Vec<TimeSpan>,
) -> Chart {
    let series_of = |t: &DynamicsTrace| -> Vec<f64> {
        let v = if short_term {
            t.short_term()
        } else {
            t.momentary()
        };
        v.into_iter().map(nan_inf).collect()
    };
    let label = if short_term {
        "Short-term loudness"
    } else {
        "Momentary loudness"
    };
    let mut series = vec![ChartSeries::new(label, track, series_of(&p.dynamics))];
    if let Some(m) = master {
        series.push(ChartSeries::new(
            format!("{label} of the mix"),
            None,
            series_of(&m.dynamics),
        ));
    }
    let lag = if short_term {
        SHORT_TERM_LAG
    } else {
        MOMENTARY_LAG
    };
    // A value stands for the window ending at its time; plotted at the
    // window's middle.
    let window = if short_term { 3.0 } else { 0.4 };
    Chart::Time {
        start: lag as f64 * LOUDNESS_STEP - window / 2.0,
        step: LOUDNESS_STEP,
        series,
        highlight,
        unit: Unit::Lufs,
        caption: format!(
            "{} (BS.1770, every 100 ms)",
            if short_term {
                "short-term, 3 s windows"
            } else {
                "momentary, 400 ms windows"
            }
        ),
    }
}

fn signal_conf(s: SignalPoint) -> f64 {
    match s {
        SignalPoint::Processed => 0.8,
        SignalPoint::Source => 0.6,
    }
}

fn loudness_range(
    input: &DiagnoseInput<'_>,
    track: TrackId,
    p: &TrackProfile,
    d: &dynamics::TrackDynamics,
    mix: Option<&dynamics::TrackDynamics>,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let rule = &input.rules.dynamics;
    let Some(lra) = d.lra_lu else {
        return;
    };
    let mix_lra = mix.and_then(|m| m.lra_lu);
    if lra < rule.track_lra_lu
        || d.active_seconds < rule.min_active_seconds
        || mix_lra.is_some_and(|m| lra < m + rule.lra_margin_lu)
    {
        return;
    }
    let mut f = blank(
        RuleId::WideLoudnessRange,
        input.signal,
        Subject::Track(track),
        &[track.0],
    );
    let t = Some(track);
    f.evidence
        .push(Evidence::new(labels::LRA, lra, Unit::Lu, t));
    if let Some([lo, mid, hi]) = d.short_term {
        f.evidence.extend([
            Evidence::new(labels::SHORT_TERM_LOW, lo, Unit::Lufs, t),
            Evidence::new(labels::SHORT_TERM_MID, mid, Unit::Lufs, t),
            Evidence::new(labels::SHORT_TERM_HIGH, hi, Unit::Lufs, t),
        ]);
    }
    f.evidence.push(Evidence::new(
        labels::ACTIVE,
        d.active_seconds,
        Unit::Seconds,
        t,
    ));
    if let Some(m) = mix_lra {
        f.evidence
            .push(Evidence::new(labels::MIX_LRA, m, Unit::Lu, None));
    } else {
        f.caveats.push(
            "The mix was not measured (an analysis before processing): the range is not \
             compared with the mix's."
                .into(),
        );
    }
    score(
        &mut f,
        vec![term(
            "range",
            (0.15 + 0.03 * (lra - rule.track_lra_lu)).clamp(0.15, 0.5),
        )],
        vec![
            term("signal", signal_conf(input.signal)),
            term("amount", 0.6 + 0.4 * (d.active_seconds / 60.0).min(1.0)),
            term("context", if mix_lra.is_some() { 1.0 } else { 0.8 }),
        ],
    );
    f.recommendations.push(Recommendation {
        experiment: Experiment::Manual {
            text: "listen to where the track is quietest and loudest against the mix; ride its \
                   fader with volume automation, or compress it gently, if it should sit more \
                   evenly"
                .into(),
        },
        purpose: "Decide whether the track's level should follow the arrangement less.".into(),
        side_effects: "Nothing is changed.".into(),
    });
    let master = input.master.map(|(_, m)| m);
    details
        .charts
        .push((f.id, loudness_chart(t, p, master, true, Vec::new())));
    out.push(f);
}

fn level_jumps(
    input: &DiagnoseInput<'_>,
    track: TrackId,
    p: &TrackProfile,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let rule = &input.rules.dynamics;
    let Some(integrated) = p
        .levels
        .loudness
        .map(|l| l.integrated)
        .filter(|v| v.is_finite())
    else {
        return;
    };
    let gate = (integrated - 20.0).max(-70.0);
    let master = input.master.map(|(_, m)| m);
    let mix_m = master.map(|m| m.dynamics.momentary());
    let span = (2.0 / LOUDNESS_STEP).round() as usize;
    let steps: Vec<(dynamics::LevelStep, Option<f64>)> =
        dynamics::level_steps(&p.dynamics, gate, rule.step_lu)
            .into_iter()
            .map(|s| {
                let k = (s.at / LOUDNESS_STEP).round() as usize;
                let mix = mix_m
                    .as_ref()
                    .and_then(|m| dynamics::step_at(m, k, span))
                    .filter(|(b, a)| b.is_finite() && a.is_finite())
                    .map(|(b, a)| a - b);
                (s, mix)
            })
            .filter(|(s, mix)| {
                mix.is_none_or(|m| {
                    // The mix moving the same way by the share: arrangement.
                    m.signum() != s.change().signum()
                        || m.abs() < rule.mirrored_share * s.change().abs()
                })
            })
            .collect();
    let Some((big, big_mix)) = steps
        .iter()
        .max_by(|x, y| x.0.change().abs().total_cmp(&y.0.change().abs()))
        .copied()
    else {
        return;
    };
    let t = Some(track);
    let mut f = blank(
        RuleId::LevelJump,
        input.signal,
        Subject::Track(track),
        &[track.0],
    );
    f.events = steps
        .iter()
        .map(|(s, _)| TimeSpan::new((s.at - 2.0).max(0.0), (s.at + 2.0).min(p.seconds())))
        .collect();
    f.time = span_of(&f.events);
    f.evidence = vec![
        Evidence::new(labels::STEP, big.change(), Unit::Lu, t),
        Evidence::new(labels::BEFORE, big.before, Unit::Lufs, t),
        Evidence::new(labels::AFTER, big.after, Unit::Lufs, t),
        Evidence::new(labels::EVENTS, steps.len() as f64, Unit::Count, t),
    ];
    if let Some(m) = big_mix {
        f.evidence
            .push(Evidence::new(labels::MIX_STEP, m, Unit::Lu, None));
    } else {
        f.caveats.push(
            "The mix was not measured (an analysis before processing): whether the whole mix \
             changes there too is not known."
                .into(),
        );
    }
    score(
        &mut f,
        vec![term(
            "step",
            (0.2 + 0.02 * (big.change().abs() - rule.step_lu)).clamp(0.2, 0.5),
        )],
        vec![
            term("signal", signal_conf(input.signal)),
            term("context", if big_mix.is_some() { 1.0 } else { 0.8 }),
        ],
    );
    f.recommendations.push(Recommendation {
        experiment: Experiment::Manual {
            text: "listen around these moments and check the clip gain, fades and volume \
                   automation there"
                .into(),
        },
        purpose: "Find whether the step is intended.".into(),
        side_effects: "Nothing is changed.".into(),
    });
    details
        .charts
        .push((f.id, loudness_chart(t, p, master, false, f.events.clone())));
    out.push(f);
}

/// The profile-only dynamics rules and every subject's summary.
pub(crate) fn run(input: &DiagnoseInput<'_>, out: &mut Vec<Finding>, details: &mut Details) {
    let rule = &input.rules.dynamics;
    let mix = input.master.map(|(_, m)| dynamics::measure(m, rule));
    for t in &input.tracks {
        let d = dynamics::measure(t.profile, rule);
        if t.focus {
            loudness_range(input, t.track, t.profile, &d, mix.as_ref(), out, details);
            level_jumps(input, t.track, t.profile, out, details);
        }
        details.dynamics.push(DynamicsSummary {
            subject: Subject::Track(t.track),
            dynamics: d,
            source: None,
            flattening: None,
        });
    }
    if let (Some((track, _)), Some(m)) = (input.master, mix) {
        details.dynamics.push(DynamicsSummary {
            subject: Subject::Master(track),
            dynamics: m,
            source: None,
            flattening: None,
        });
    }
}

fn role_weight(role: Option<TrackRole>) -> f64 {
    match role.map(TrackRole::family) {
        Some(RoleFamily::DrumKit) => 1.0,
        _ if role == Some(TrackRole::Percussion) => 1.0,
        _ => 0.8,
    }
}

/// Possible transient flattening of `track` (processed `p`) against its
/// source profile `source`; its summary completed either way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flattening(
    input: &DiagnoseInput<'_>,
    track: TrackId,
    role: Option<TrackRole>,
    p: &TrackProfile,
    source: &TrackProfile,
    sends: bool,
    master_devices: bool,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let rule = &input.rules.dynamics;
    let src = dynamics::measure(source, rule);
    let flat = dynamics::flattening(&source.dynamics, &p.dynamics, rule);
    let proc_crest = dynamics::measure(p, rule).crest_db;
    if let Some(d) = details
        .dynamics
        .iter_mut()
        .find(|d| d.subject == Subject::Track(track))
    {
        d.source = Some(src.clone());
        d.flattening = flat;
    }
    let Some(fl) = flat else {
        return;
    };
    if input.signal != SignalPoint::Processed
        || fl.source_tsr_db < rule.min_source_tsr_db
        || fl.tsr_change_db > -rule.flatten_db
    {
        return;
    }
    let t = Some(track);
    let mut f = blank(
        RuleId::TransientFlattening,
        input.signal,
        Subject::Track(track),
        &[track.0],
    );
    let crest_change = src.crest_db.zip(proc_crest).map(|(a, b)| b - a);
    f.evidence = vec![
        Evidence::new(labels::TSR_SOURCE, fl.source_tsr_db, Unit::Db, t),
        Evidence::new(labels::TSR_PROCESSED, fl.processed_tsr_db, Unit::Db, t),
        Evidence::new(labels::TSR_CHANGE, fl.tsr_change_db, Unit::Db, t),
        Evidence::new(labels::ATTACK_CHANGE, fl.attack_change_db, Unit::Db, t),
        Evidence::new(labels::EVENTS, fl.events as f64, Unit::Count, t),
    ];
    if let Some(c) = src.crest_db {
        f.evidence
            .push(Evidence::new(labels::CREST_SOURCE, c, Unit::Db, t));
    }
    if let Some(c) = proc_crest {
        f.evidence
            .push(Evidence::new(labels::CREST, c, Unit::Db, t));
    }
    if let Some(c) = crest_change {
        f.evidence
            .push(Evidence::new(labels::CREST_CHANGE, c, Unit::Db, t));
    }
    if sends {
        f.caveats.push(
            "The track feeds sends: their returns (reverb, delay) are part of its processed \
             signal and add sustain, which lowers the ratio without any compression."
                .into(),
        );
    }
    if master_devices {
        f.caveats.push(
            "The master (or a bus on the way) has devices: rendered with this track alone, a \
             limiter or compressor there reacts differently than with the whole mix."
                .into(),
        );
    }
    let drop = -fl.tsr_change_db;
    score(
        &mut f,
        vec![
            term("role", role_weight(role)),
            term(
                "drop",
                (0.2 + (drop - rule.flatten_db) / 10.0).clamp(0.2, 0.7),
            ),
        ],
        vec![
            term("signal", 0.85),
            term("amount", 0.6 + 0.4 * (fl.events as f64 / 24.0).min(1.0)),
            term("sends", if sends { 0.7 } else { 1.0 }),
            term(
                "agreement",
                if crest_change.is_some_and(|c| c <= -1.0) {
                    1.0
                } else {
                    0.85
                },
            ),
        ],
    );
    f.recommendations.push(Recommendation {
        experiment: Experiment::Manual {
            text: "bypass the track's dynamics devices one at a time (and compare with the \
                   master's or a bus's limiter bypassed) to hear where the attacks go; a slower \
                   attack, less gain reduction or a parallel blend keeps more of them"
                .into(),
        },
        purpose: "Find whether the attacks are flattened on purpose.".into(),
        side_effects: "Nothing is changed.".into(),
    });
    out.push(f);
}
