//! [`RuleId::SameAudio`]: two tracks carrying the same audio
//! ([`crate::same`]) for at least [`crate::config::SameRule::min_seconds`].
//! The masking rule leaves those stretches out (a copy cannot make itself
//! less distinct); this finding says where and why, with the level
//! between the copies and what their mono sum does. Pairs whose roles
//! already say they are one source (parts of one drum kit) are not
//! compared for masking, so not reported here either.
//!
//! Severity `0.15 + 0.15 · clamp((coherence − 0.9) / 0.08, 0, 1)` (low:
//! information; near-identical copies weigh a little more, they may be an
//! accidental duplicate); confidence signal (processed 0.9, source 0.8) ×
//! amount (`0.6 + 0.4 · min(1, seconds / 8)`) × the coherence.

use crate::details::{Chart, ChartSeries};
use crate::finding::{
    Evidence, Experiment, Finding, Recommendation, RuleId, SignalPoint, Subject, Unit, labels,
};
use crate::rulekit::{blank, score, span_of, term};
use crate::rules::{DiagnoseInput, TrackInput};
use crate::same::SameAudio;

/// The finding for a pair (`None`: too short to report).
pub(crate) fn finding(
    input: &DiagnoseInput<'_>,
    a: &TrackInput<'_>,
    b: &TrackInput<'_>,
    s: &SameAudio,
    charts: &mut Vec<(u64, Chart)>,
) -> Option<Finding> {
    let rule = &input.rules.same;
    if s.seconds < rule.min_seconds {
        return None;
    }
    let mut f = blank(
        RuleId::SameAudio,
        input.signal,
        Subject::Tracks {
            a: a.track,
            b: b.track,
        },
        &[a.track.0, b.track.0],
    );
    f.events = s.spans.clone();
    f.time = span_of(&f.events);
    f.evidence = vec![
        Evidence::new(labels::SAME_SECONDS, s.seconds, Unit::Seconds, None),
        Evidence::new(labels::COHERENCE, s.coherence, Unit::Ratio, None),
        Evidence::new(labels::BAND_COHERENCE, s.band_coherence, Unit::Ratio, None),
        Evidence::new(labels::SAME_BANDS, s.bands, Unit::Count, None),
        Evidence::new(labels::LEVEL_BETWEEN, s.level_db, Unit::Db, Some(b.track)),
        Evidence::new(labels::MONO_SUM, s.mono_gain_db, Unit::Db, None),
    ];
    if s.lag.abs() >= 0.25 {
        let rate = f64::from(a.profile.rate.max(1));
        f.evidence.push(Evidence::new(
            labels::SAME_LAG,
            s.lag / rate * 1000.0,
            Unit::Ms,
            Some(b.track),
        ));
    }
    let signal = match input.signal {
        SignalPoint::Processed => 0.9,
        SignalPoint::Source => 0.8,
    };
    score(
        &mut f,
        vec![term(
            "near-identity",
            0.15 + 0.15 * ((s.coherence - 0.9) / 0.08).clamp(0.0, 1.0),
        )],
        vec![
            term("signal", signal),
            term("amount", 0.6 + 0.4 * (s.seconds / 8.0).min(1.0)),
            term("coherence", s.coherence.clamp(0.0, 1.0)),
        ],
    );
    if input.signal == SignalPoint::Source {
        f.caveats.push(
            "Measured before processing: the mix's devices may make the two differ (or not)."
                .into(),
        );
    }
    f.recommendations.push(Recommendation {
        experiment: Experiment::Manual {
            text: "check whether the second track is meant to be a copy: for width, a \
                   different performance or a stereo effect on one of them does that; for \
                   level, one track's fader does the same as a copy"
                .into(),
        },
        purpose: "A copy adds level and moves the image; it does not add a second sound.".into(),
        side_effects: "Nothing is changed.".into(),
    });
    charts.push((
        f.id,
        Chart::Bands {
            centres: a.profile.layout.bands().iter().map(|x| x.centre).collect(),
            series: vec![ChartSeries::new("Coherence", None, s.per_band.clone())],
            highlight: None,
            unit: Unit::Ratio,
            zero: false,
            caption: "coherence per band where they carry the same audio (1 = linearly related)"
                .into(),
        },
    ));
    Some(f)
}
