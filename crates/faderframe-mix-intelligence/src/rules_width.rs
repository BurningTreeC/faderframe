//! The width rules (§ stereo, frequency-dependent): a wide low end
//! ([`RuleId::WideLowEnd`]) and the master's balance in mono
//! ([`RuleId::MonoBalanceShift`]). Thresholds in
//! [`crate::config::WidthRule`].
//!
//! * Wide low end — per subject (focus tracks, the master; stereo only):
//!   bands under `low_end_hz`, relevant (within `relevance_db` of the
//!   loudest, above −60 dB), whose long-term width is at least `wide_db`
//!   and whose frames are that wide for `min_seconds`; contiguous bands
//!   make ranges, the widest is reported unless a mono fold-down loss
//!   already covers it. Severity `0.2 + 0.3 · clamp((width − wide_db) /
//!   6, 0, 1)`; confidence signal (processed 0.85, source 0.65) × amount
//!   (`0.6 + 0.4 · min(1, seconds / 8)`).
//! * Mono balance shift — the master (processed): relevant bands whose
//!   shift ([`crate::width`]) is `mono_shift_db` or more down; the range
//!   losing most is reported unless a mono fold-down loss covers it, with
//!   the tracks bringing the most side signal there (each measured alone,
//!   a share of all tracks' side power in the range). Severity
//!   `clamp(−shift / 12, 0.15, 0.6)`; confidence 0.8 × resolution (0.85
//!   for ranges under 60 Hz).

use crate::details::{Chart, ChartSeries, Details};
use crate::finding::{
    Evidence, Experiment, Finding, FrequencyRange, Recommendation, RuleId, SignalPoint, Subject,
    Unit, labels,
};
use crate::profile::{TrackProfile, power_db};
use crate::rulekit::{blank, key_of, round_to, score, term, track_of};
use crate::rules::DiagnoseInput;
use crate::stereo::BandStereo;
use crate::width::{self, BandWidth};

fn covered(out: &[Finding], subject: Subject, lo: f64, hi: f64) -> bool {
    out.iter().any(|f| {
        f.rule == RuleId::MonoFoldDownLoss
            && f.subject == subject
            && f.frequency.is_some_and(|r| r.lo < hi && lo < r.hi)
    })
}

/// Contiguous runs of `on` (band index ranges).
fn runs(on: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut b = 0;
    while b < on.len() {
        if !on[b] {
            b += 1;
            continue;
        }
        let a = b;
        while b < on.len() && on[b] {
            b += 1;
        }
        out.push((a, b - 1));
    }
    out
}

fn width_chart(
    bands: &[BandWidth],
    track: Option<faderframe_core::TrackId>,
    hl: (usize, usize),
) -> Chart {
    let nan = |v: Option<f64>| v.unwrap_or(f64::NAN);
    Chart::Bands {
        centres: bands.iter().map(|b| b.centre).collect(),
        series: vec![
            ChartSeries::new(
                "Side against mid",
                track,
                bands.iter().map(|b| nan(b.width_db())).collect(),
            ),
            ChartSeries::new(
                "Mono change",
                track,
                bands.iter().map(|b| nan(b.mono_db())).collect(),
            ),
        ],
        highlight: Some(hl),
        unit: Unit::Db,
        zero: true,
        caption: "dB per third octave, the whole range".into(),
    }
}

fn wide_low_end(
    input: &DiagnoseInput<'_>,
    subject: Subject,
    p: &TrackProfile,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let rule = &input.rules.width;
    let b = width::bands(p);
    let rel = width::relevant(&b, rule.relevance_db, -60.0);
    let top = b
        .iter()
        .map(BandWidth::level_db)
        .fold(f64::NEG_INFINITY, f64::max);
    let hop = p.hop_seconds();
    let seconds_wide = |i: usize| -> f64 {
        (0..p.frames.frames())
            .filter(|&t| {
                p.frames.level(t, i) >= top - rule.relevance_db && {
                    let s = BandStereo::of_frame(p, t, i);
                    power_db(s.side) - power_db(s.mid) >= rule.wide_db
                }
            })
            .count() as f64
            * hop
    };
    let wide: Vec<bool> = b
        .iter()
        .enumerate()
        .map(|(i, x)| {
            x.centre < rule.low_end_hz
                && rel[i]
                && x.width_db().is_some_and(|w| w >= rule.wide_db)
                && seconds_wide(i) >= rule.min_seconds
        })
        .collect();
    let range_width = |(a, z): (usize, usize)| -> f64 {
        let (mid, side) = b[a..=z]
            .iter()
            .fold((0.0, 0.0), |acc, x| (acc.0 + x.mid, acc.1 + x.side));
        power_db(side) - power_db(mid)
    };
    let Some((a, z)) = runs(&wide)
        .into_iter()
        .max_by(|x, y| range_width(*x).total_cmp(&range_width(*y)).then(y.cmp(x)))
    else {
        return;
    };
    let (lo, hi) = (b[a].lo, b[z].hi);
    if covered(out, subject, lo, hi) {
        return;
    }
    let w = range_width((a, z));
    let secs = (a..=z).map(seconds_wide).fold(0.0, f64::max);
    let track = track_of(subject);
    let mut f = blank(
        RuleId::WideLowEnd,
        input.signal,
        subject,
        &[key_of(subject), a as u64],
    );
    f.frequency = Some(FrequencyRange { lo, hi });
    let (mid, power) = b[a..=z]
        .iter()
        .fold((0.0, 0.0), |acc, x| (acc.0 + x.mid, acc.1 + x.power));
    f.evidence = vec![
        Evidence::new(labels::WIDTH, w, Unit::Db, track),
        Evidence::new(
            labels::MONO_CHANGE,
            power_db(mid) - power_db(power),
            Unit::Db,
            track,
        ),
        Evidence::new(labels::BAND_LEVEL, power_db(power), Unit::DbBand, track),
        Evidence::new(labels::WIDE_SECONDS, secs, Unit::Seconds, None),
    ];
    let signal = match input.signal {
        SignalPoint::Processed => 0.85,
        SignalPoint::Source => 0.65,
    };
    score(
        &mut f,
        vec![term(
            "width",
            0.2 + 0.3 * ((w - rule.wide_db) / 6.0).clamp(0.0, 1.0),
        )],
        vec![
            term("signal", signal),
            term("amount", 0.6 + 0.4 * (secs / 8.0).min(1.0)),
        ],
    );
    if let Some(t) = track {
        f.recommendations.push(Recommendation {
            experiment: Experiment::SideLowCut {
                track: t,
                freq_hz: round_to(hi.clamp(60.0, 250.0), 5.0),
            },
            purpose: "Make the low end mono below the range's top (the EQ's band on Side), so \
                      mono playback and summed subwoofers keep it as it is in stereo."
                .into(),
            side_effects: "Narrows the image under that frequency: an intended wide or panned \
                           low end goes."
                .into(),
        });
    }
    f.recommendations.push(Recommendation {
        experiment: Experiment::Manual {
            text: "listen with the master's MONO button, and look for a stereo widener, chorus, \
                   panned low instrument or spaced microphones in this range"
                .into(),
        },
        purpose: "Find what makes the low end wide.".into(),
        side_effects: "Nothing is changed.".into(),
    });
    if input.signal == SignalPoint::Source {
        f.caveats.push(
            "Measured before processing: panning and the mix's devices may change this.".into(),
        );
    }
    details.charts.push((f.id, width_chart(&b, track, (a, z))));
    out.push(f);
}

fn mono_balance(
    input: &DiagnoseInput<'_>,
    subject: Subject,
    p: &TrackProfile,
    out: &mut Vec<Finding>,
    details: &mut Details,
) {
    let rule = &input.rules.width;
    let b = width::bands(p);
    let rel = width::relevant(&b, rule.relevance_db, -60.0);
    let Some(whole) = width::whole_mono_db(&b, &rel) else {
        return;
    };
    let shift = |x: &BandWidth| x.mono_db().map(|m| m - whole);
    let down: Vec<bool> = b
        .iter()
        .zip(&rel)
        .map(|(x, r)| *r && shift(x).is_some_and(|s| s <= -rule.mono_shift_db))
        .collect();
    let range_shift = |(a, z): (usize, usize)| -> f64 {
        let (mid, power) = b[a..=z]
            .iter()
            .fold((0.0, 0.0), |acc, x| (acc.0 + x.mid, acc.1 + x.power));
        power_db(mid) - power_db(power) - whole
    };
    let Some((a, z)) = runs(&down)
        .into_iter()
        .min_by(|x, y| range_shift(*x).total_cmp(&range_shift(*y)).then(x.cmp(y)))
    else {
        return;
    };
    let (lo, hi) = (b[a].lo, b[z].hi);
    if covered(out, subject, lo, hi) {
        return;
    }
    let s = range_shift((a, z));
    let track = track_of(subject);
    let mut f = blank(
        RuleId::MonoBalanceShift,
        input.signal,
        subject,
        &[key_of(subject), a as u64],
    );
    f.frequency = Some(FrequencyRange { lo, hi });
    f.evidence = vec![
        Evidence::new(labels::MONO_SHIFT, s, Unit::Db, track),
        Evidence::new(labels::MONO_CHANGE, s + whole, Unit::Db, track),
        Evidence::new(labels::WHOLE_MONO, whole, Unit::Db, track),
    ];
    // The tracks bringing the most side signal there (each alone).
    let sides: Vec<(faderframe_core::TrackId, f64)> = input
        .tracks
        .iter()
        .filter(|t| t.profile.channels >= 2 && t.profile.layout == p.layout)
        .map(|t| {
            let side: f64 = width::bands(t.profile)[a..=z].iter().map(|x| x.side).sum();
            (t.track, side)
        })
        .collect();
    let total: f64 = sides.iter().map(|x| x.1).sum();
    let mut top: Vec<(faderframe_core::TrackId, f64)> = sides
        .into_iter()
        .filter(|x| total > 0.0 && x.1 / total >= 0.1)
        .map(|(t, v)| (t, v / total))
        .collect();
    top.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    top.truncate(3);
    for (t, share) in &top {
        f.evidence.push(Evidence::new(
            labels::SIDE_SHARE,
            *share,
            Unit::Share,
            Some(*t),
        ));
        f.tracks.push(*t);
    }
    let resolution = if lo < 60.0 { 0.85 } else { 1.0 };
    score(
        &mut f,
        vec![term("shift", (-s / 12.0).clamp(0.15, 0.6))],
        vec![term("signal", 0.8), term("resolution", resolution)],
    );
    if let Some(&(t, share)) = top.first()
        && share >= 0.5
        && hi <= 300.0
    {
        f.recommendations.push(Recommendation {
            experiment: Experiment::SideLowCut {
                track: t,
                freq_hz: round_to(hi.clamp(60.0, 250.0), 5.0),
            },
            purpose: "Narrow the track bringing most of the side signal there, below the \
                      range's top, so the mono mix keeps that range."
                .into(),
            side_effects: "That track's low end becomes mono; the stereo image there narrows."
                .into(),
        });
    }
    f.recommendations.push(Recommendation {
        experiment: Experiment::Manual {
            text: "compare with the master's MONO button; if the range matters in mono, narrow \
                   or re-balance the widest tracks there (their stereo wideners, choruses, \
                   spaced microphones)"
                .into(),
        },
        purpose: "Decide whether the mono balance matters for this mix.".into(),
        side_effects: "Nothing is changed.".into(),
    });
    details.charts.push((f.id, width_chart(&b, track, (a, z))));
    out.push(f);
}

/// The width rules on the focus tracks and the master; the width
/// summaries of every stereo subject.
pub(crate) fn run(input: &DiagnoseInput<'_>, out: &mut Vec<Finding>, details: &mut Details) {
    for t in &input.tracks {
        if let Some(s) = width::summary(Subject::Track(t.track), t.profile) {
            details.width.push(s);
        }
        if t.focus && t.profile.channels >= 2 {
            wide_low_end(input, Subject::Track(t.track), t.profile, out, details);
        }
    }
    if let Some((track, p)) = input.master {
        let subject = Subject::Master(track);
        if let Some(s) = width::summary(subject, p) {
            details.width.push(s);
        }
        if p.channels >= 2 {
            wide_low_end(input, subject, p, out, details);
            if input.signal == SignalPoint::Processed {
                mono_balance(input, subject, p, out, details);
            }
        }
    }
}
