//! The phase, timing, width and dynamics rules on synthetic signals with
//! known relationships: true positives and false positives.

use crate::config::AnalysisConfig;
use crate::dynamics::tests::{hits, squash};
use crate::finding::{Experiment, RuleId, SignalPoint, Subject, labels};
use crate::phase::tests::{Noise, delayed, drums};
use crate::phase::{PairSettings, candidates, excerpts, measure_pair};
use crate::rules::{DiagnoseInput, Report, TrackInput, diagnose};
use crate::rules_phase::{ExtraInput, PairInput, SourceInput, diagnose_extra};
use crate::test_support::{SR, profile, tone};
use crate::{Finding, Rules, TrackProfile};
use faderframe_core::{TrackId, TrackRole};
use std::f64::consts::TAU;

/// A track of the tests: id, role, audio (planar).
struct T {
    id: u64,
    role: Option<TrackRole>,
    audio: Vec<Vec<f32>>,
}

fn t(id: u64, role: Option<TrackRole>, audio: Vec<Vec<f32>>) -> T {
    T { id, role, audio }
}

fn prof(x: &T) -> TrackProfile {
    let ch: Vec<&[f32]> = x.audio.iter().map(Vec::as_slice).collect();
    profile(&ch)
}

/// Diagnose `tracks` (processed, all in focus) and their pairs as the
/// session does: candidates, excerpts, measurements, the extra rules.
fn run_pairs(tracks: &[T], selected: &[u64]) -> Report {
    let rules = Rules::default();
    let profiles: Vec<TrackProfile> = tracks.iter().map(prof).collect();
    let input = DiagnoseInput {
        tracks: tracks
            .iter()
            .zip(&profiles)
            .map(|(x, p)| TrackInput {
                track: TrackId(x.id),
                role: x.role,
                profile: p,
                compare: true,
                focus: true,
            })
            .collect(),
        master: None,
        signal: SignalPoint::Processed,
        sections: Vec::new(),
        rules: &rules,
        cancel: None,
        related: Vec::new(),
    };
    let mut report = diagnose(&input).unwrap_or_default();
    let selected: Vec<TrackId> = selected.iter().map(|i| TrackId(*i)).collect();
    let cands = candidates(&input.tracks, &report.findings, &selected, &rules.phase);
    let of = |id: TrackId| {
        tracks
            .iter()
            .position(|x| TrackId(x.id) == id)
            .map(|i| &profiles[i])
    };
    let ex = excerpts(&cands, &of, &rules.phase);
    let config = AnalysisConfig::for_rate(SR);
    let settings = PairSettings::of(&rules);
    let mut measured = Vec::new();
    for (c, e) in cands.iter().zip(&ex) {
        let Some(e) = e else { continue };
        let cut = |id: TrackId| -> Vec<&[f32]> {
            let x = tracks.iter().find(|x| TrackId(x.id) == id);
            x.map(|x| {
                x.audio
                    .iter()
                    .map(|c| &c[e.start..e.start + e.frames])
                    .collect()
            })
            .unwrap_or_default()
        };
        let ph = measure_pair(
            &cut(c.a),
            &cut(c.b),
            SR,
            &config,
            &settings,
            e.start as f64 / f64::from(SR),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        measured.push((c.clone(), ph));
    }
    let extra = ExtraInput {
        pairs: measured
            .iter()
            .map(|(c, ph)| PairInput {
                candidate: c,
                phase: ph,
            })
            .collect(),
        ..ExtraInput::default()
    };
    diagnose_extra(&input, &extra, &mut report).unwrap_or_else(|_| panic!("cancelled"));
    report
}

fn of_rule(r: &Report, rule: RuleId) -> Vec<&Finding> {
    r.findings.iter().filter(|f| f.rule == rule).collect()
}

fn phase_findings(r: &Report) -> Vec<&Finding> {
    r.findings
        .iter()
        .filter(|f| f.category() == crate::Category::Phase)
        .collect()
}

fn ev(f: &Finding, label: &str) -> f64 {
    f.evidence
        .iter()
        .find(|e| e.label == label)
        .map_or(f64::NAN, |e| e.value)
}

fn alignment(f: &Finding) -> Option<(TrackId, f64, bool)> {
    f.recommendations.iter().find_map(|r| match r.experiment {
        Experiment::Alignment {
            track,
            change_ms,
            invert,
        } => Some((track, change_ms, invert)),
        _ => None,
    })
}

const LEN: f64 = 12.0;

#[test]
fn a_snare_top_and_bottom_are_found_inverted_and_turned_over() {
    let top = drums(LEN, 0.25, 21);
    // The bottom microphone: the other way up, quieter, 0.6 ms later.
    let bottom: Vec<f32> = delayed(&top, 0.6 * 48.0).iter().map(|v| -0.8 * v).collect();
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Snare), vec![top.clone()]),
            t(2, Some(TrackRole::Snare), vec![bottom]),
        ],
        &[],
    );
    let p = of_rule(&r, RuleId::PolarityInverted);
    assert_eq!(p.len(), 1, "{:#?}", r.findings);
    let f = p[0];
    assert_eq!(
        f.subject,
        Subject::Tracks {
            a: TrackId(1),
            b: TrackId(2)
        }
    );
    assert!(ev(f, labels::SUM_NOW) < -5.0, "{}", ev(f, labels::SUM_NOW));
    assert!(ev(f, labels::SUM_CORRECTED) > 2.5);
    assert!(
        (ev(f, labels::LAG) - 0.6).abs() < 0.01,
        "{}",
        ev(f, labels::LAG)
    );
    // The later one moves earlier by the delay and turns over; the
    // polarity alone is the alternative.
    let (track, change, invert) = alignment(f).unwrap_or((TrackId(0), 0.0, false));
    assert_eq!(track, TrackId(2));
    assert!((change + 0.6).abs() < 0.01 && invert, "{change}");
    assert!(f.recommendations.iter().any(|x| matches!(
        x.experiment,
        Experiment::Alignment { change_ms, invert: true, .. } if change_ms == 0.0
    )));
    for x in &f.recommendations {
        assert!(x.experiment.validate(24_000.0).is_ok());
    }
    assert!(f.confidence > 0.7, "{}", f.confidence);
    // Nothing else is said about this pair: the correction explains it.
    assert!(of_rule(&r, RuleId::TimeOffset).is_empty());
    assert!(of_rule(&r, RuleId::FrequencyDependentPhase).is_empty());
    // A chart of the sum per band, and the pair in the details.
    assert!(r.details.chart(f.id).is_some());
    assert_eq!(r.details.pairs.len(), 1);
    // Deterministic.
    let again = run_pairs(
        &[
            t(1, Some(TrackRole::Snare), vec![top.clone()]),
            t(
                2,
                Some(TrackRole::Snare),
                vec![delayed(&top, 0.6 * 48.0).iter().map(|v| -0.8 * v).collect()],
            ),
        ],
        &[],
    );
    assert_eq!(again.findings, r.findings);
}

#[test]
fn a_close_microphone_is_delayed_to_the_room_but_an_echo_is_left_alone() {
    let close = drums(LEN, 0.25, 22);
    // The kit's room microphone hears it 1.3 ms later (62.4 samples).
    let room: Vec<f32> = delayed(&close, 1.3 * 48.0)
        .iter()
        .map(|v| 0.7 * v)
        .collect();
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Kick), vec![close.clone()]),
            t(2, Some(TrackRole::Drums), vec![room]),
        ],
        &[],
    );
    let o = of_rule(&r, RuleId::TimeOffset);
    assert_eq!(o.len(), 1, "{:#?}", r.findings);
    let f = o[0];
    assert!((ev(f, labels::LAG) - 1.3).abs() < 0.01);
    assert!((ev(f, labels::LAG_SAMPLES) - 62.4).abs() < 0.1);
    // The first notch at 1/(2·1.3 ms).
    assert!((ev(f, labels::FIRST_NOTCH) - 384.6).abs() < 1.0);
    assert!(ev(f, labels::GAIN) >= 1.0 || ev(f, labels::WORST_BAND_SUM) < -3.0);
    // The room keeps its place (the kit's reference); the close one is
    // delayed to it.
    let (track, change, invert) = alignment(f).unwrap_or((TrackId(0), 0.0, true));
    assert_eq!(track, TrackId(1));
    assert!((change - 1.3).abs() < 0.01 && !invert, "{change}");
    // The caveat about musical offsets, and a manual alternative.
    assert!(
        f.rule
            .limitations()
            .iter()
            .any(|l| l.contains("highest correlation"))
    );
    assert!(
        f.recommendations
            .iter()
            .any(|x| matches!(x.experiment, Experiment::Manual { .. }))
    );
    // An echo 40 ms later is not comb filtering: no delay or polarity
    // finding, and no move of more than the limit is suggested.
    let echo: Vec<f32> = delayed(&close, 0.040 * 48_000.0)
        .iter()
        .map(|v| 0.6 * v)
        .collect();
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Kick), vec![close.clone()]),
            t(2, Some(TrackRole::Drums), vec![echo]),
        ],
        &[],
    );
    assert!(
        of_rule(&r, RuleId::TimeOffset).is_empty(),
        "{:#?}",
        r.findings
    );
    assert!(of_rule(&r, RuleId::PolarityInverted).is_empty());
    for f in phase_findings(&r) {
        if let Some((_, change, _)) = alignment(f) {
            assert!(change.abs() <= 30.0, "{f:#?}");
        }
    }
    // Two identical microphones: nothing to correct.
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Kick), vec![close.clone()]),
            t(2, Some(TrackRole::Drums), vec![close]),
        ],
        &[],
    );
    assert!(phase_findings(&r).is_empty(), "{:#?}", r.findings);
    assert_eq!(r.details.pairs.len(), 1, "compared, all well");
}

#[test]
fn unrelated_parts_and_steady_tones_find_nothing() {
    // A kick and a bass whose pitch drifts against the beat: no shared
    // signal, no shared onsets.
    let kick = hits(0.8, 0.5, LEN);
    let bass = tone(61.3, 0.3, 0.0, LEN, LEN);
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Kick), vec![kick]),
            t(2, Some(TrackRole::Bass), vec![bass]),
        ],
        &[],
    );
    assert_eq!(r.details.pairs.len(), 1, "kick and bass are compared");
    assert!(phase_findings(&r).is_empty(), "{:#?}", phase_findings(&r));
    // Two noises, selected together: compared, nothing found.
    let mut n1 = Noise(31);
    let mut n2 = Noise(32);
    let a: Vec<f32> = (0..(LEN * 48_000.0) as usize)
        .map(|_| (0.2 * n1.next()) as f32)
        .collect();
    let b: Vec<f32> = (0..(LEN * 48_000.0) as usize)
        .map(|_| (0.2 * n2.next()) as f32)
        .collect();
    let r = run_pairs(&[t(1, None, vec![a.clone()]), t(2, None, vec![b])], &[1, 2]);
    assert_eq!(r.details.pairs.len(), 1);
    let s = &r.details.pairs[0];
    assert!(s.sum_db.unwrap_or(9.0).abs() < 0.3, "{s:?}");
    assert!(phase_findings(&r).is_empty());
    // Without roles, selection or masking, unrelated steady tracks are
    // not even compared.
    let r = run_pairs(
        &[
            t(1, None, vec![tone(300.0, 0.3, 0.0, LEN, LEN)]),
            t(2, None, vec![tone(2000.0, 0.3, 0.0, LEN, LEN)]),
        ],
        &[],
    );
    assert!(r.details.pairs.is_empty());
    // Silence: nothing at all.
    let z = vec![0.0f32; (LEN * 48_000.0) as usize];
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Kick), vec![z.clone()]),
            t(2, Some(TrackRole::Kick), vec![z]),
        ],
        &[],
    );
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
}

/// A kick and a layered sub at its pitch, 150° apart there: they cancel in
/// that band; no delay explains it.
#[test]
fn a_layered_sub_cancelling_at_its_pitch_is_frequency_dependent() {
    let rate = f64::from(SR);
    let n = (LEN * rate) as usize;
    let mut click = Noise(41);
    let (mut kick, mut sub) = (vec![0.0f32; n], vec![0.0f32; n]);
    for i in 0..n {
        let s = (i as f64 / rate) % 0.5;
        let env = (-s * 6.0).exp();
        kick[i] =
            (0.5 * env * (TAU * 55.0 * s).sin() + 0.3 * click.next() * (-s * 300.0).exp()) as f32;
        sub[i] = (0.5 * env * (TAU * 55.0 * s + 150f64.to_radians()).sin()) as f32;
    }
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::Kick), vec![kick]),
            t(2, Some(TrackRole::Kick), vec![sub]),
        ],
        &[],
    );
    let f = of_rule(&r, RuleId::FrequencyDependentPhase);
    assert_eq!(f.len(), 1, "{:#?}", r.findings);
    let f = f[0];
    let range = f
        .frequency
        .unwrap_or(crate::FrequencyRange { lo: 0.0, hi: 0.0 });
    assert!(range.lo <= 55.0 && range.hi >= 55.0, "{range:?}");
    assert!(ev(f, labels::SUM_NOW) < -5.0, "{}", ev(f, labels::SUM_NOW));
    assert!((ev(f, labels::PHASE).abs() - 150.0).abs() < 15.0);
    assert!(ev(f, labels::COHERENCE) > 0.8);
    // Flipping helps there (measured, offered); the all-pass is a manual
    // suggestion only.
    assert!(ev(f, labels::SUM_FLIPPED) > 1.0);
    assert!(f.recommendations.iter().any(|x| matches!(
        x.experiment,
        Experiment::Alignment { invert: true, change_ms, .. } if change_ms == 0.0
    )));
    assert!(f.recommendations.iter().any(|x| matches!(
        &x.experiment,
        Experiment::Manual { text } if text.contains("All Pass")
    )));
    assert!(of_rule(&r, RuleId::TimeOffset).is_empty());
    assert!(of_rule(&r, RuleId::PolarityInverted).is_empty());
}

#[test]
fn doubled_parts_flam_or_scatter_and_tight_ones_do_not() {
    let rate = f64::from(SR);
    let one = drums(LEN, 0.5, 51);
    // Another performance, 12 ms late every time.
    let late = delayed(&drums(LEN, 0.5, 52), 0.012 * rate);
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::RhythmGuitar), vec![one.clone()]),
            t(2, Some(TrackRole::RhythmGuitar), vec![late]),
        ],
        &[],
    );
    let f = of_rule(&r, RuleId::OnsetOffset);
    assert_eq!(f.len(), 1, "{:#?}", r.findings);
    let f = f[0];
    assert!((ev(f, labels::ONSET_MEDIAN) - 12.0).abs() < 1.5);
    let (track, change, invert) = alignment(f).unwrap_or((TrackId(0), 0.0, true));
    assert_eq!(track, TrackId(2), "the later one moves");
    assert!((change + 12.0).abs() < 1.5 && !invert, "{change}");
    assert!(
        f.rule.limitations().iter().any(|l| l.contains("feel")),
        "says the offset may be the groove"
    );
    assert!(matches!(
        r.details.chart(f.id),
        Some(crate::Chart::Offsets { .. })
    ));
    // The kick partials the flam makes cancel are explained by it: no
    // separate frequency-dependent finding; what moving would bring back is
    // measured.
    assert!(
        of_rule(&r, RuleId::FrequencyDependentPhase).is_empty(),
        "{:#?}",
        of_rule(&r, RuleId::FrequencyDependentPhase)
    );
    assert!(ev(f, labels::SUM_CORRECTED).is_finite());
    // Loose: each hit off by its own ±30 ms.
    let mut jitter = Noise(53);
    let hit = drums(0.5, 1.0, 54);
    let mut loose = vec![0.0f32; one.len()];
    for k in 0..(LEN / 0.5) as usize {
        let at = ((k as f64 * 0.5 + 0.03 * jitter.next()) * rate).max(0.0) as usize;
        for (i, v) in hit.iter().enumerate() {
            if at + i < loose.len() {
                loose[at + i] += v;
            }
        }
    }
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::RhythmGuitar), vec![one.clone()]),
            t(2, Some(TrackRole::RhythmGuitar), vec![loose]),
        ],
        &[],
    );
    let f = of_rule(&r, RuleId::LooseTiming);
    assert_eq!(f.len(), 1, "{:#?}", r.details.pairs);
    assert!(alignment(f[0]).is_none(), "loose timing is edited by hand");
    // Tight: another performance 1 ms apart: nothing.
    let tight = delayed(&drums(LEN, 0.5, 55), 0.001 * rate);
    let r = run_pairs(
        &[
            t(1, Some(TrackRole::RhythmGuitar), vec![one]),
            t(2, Some(TrackRole::RhythmGuitar), vec![tight]),
        ],
        &[],
    );
    assert!(
        of_rule(&r, RuleId::OnsetOffset).is_empty(),
        "{:#?}",
        r.findings
    );
    assert!(of_rule(&r, RuleId::LooseTiming).is_empty());
}

fn stereo_sine(hz: f64, amp: f64, shift: f64) -> (Vec<f32>, Vec<f32>) {
    let rate = f64::from(SR);
    let n = (LEN * rate) as usize;
    (
        (0..n)
            .map(|i| (amp * (TAU * hz * i as f64 / rate).sin()) as f32)
            .collect(),
        (0..n)
            .map(|i| (amp * (TAU * hz * i as f64 / rate - shift).sin()) as f32)
            .collect(),
    )
}

fn run_with_master(tracks: &[T], master: Option<&T>, signal: SignalPoint) -> Report {
    let rules = Rules::default();
    let profiles: Vec<TrackProfile> = tracks.iter().map(prof).collect();
    let mp = master.map(prof);
    let input = DiagnoseInput {
        tracks: tracks
            .iter()
            .zip(&profiles)
            .map(|(x, p)| TrackInput {
                track: TrackId(x.id),
                role: x.role,
                profile: p,
                compare: true,
                focus: true,
            })
            .collect(),
        master: mp.as_ref().map(|p| (Some(TrackId(99)), p)),
        signal,
        sections: Vec::new(),
        rules: &rules,
        cancel: None,
        related: Vec::new(),
    };
    diagnose(&input).unwrap_or_default()
}

#[test]
fn a_wide_low_end_is_reported_and_wide_highs_are_not() {
    // 60 Hz at a correlation of 0.3: wide, but loses under 3 dB in mono.
    let (l, r) = stereo_sine(60.0, 0.3, 0.3f64.acos());
    let wide = t(1, None, vec![l, r]);
    // Uncorrelated 1 kHz: as wide, but high up.
    let (l, r) = stereo_sine(1000.0, 0.3, std::f64::consts::FRAC_PI_2);
    let high = t(2, None, vec![l, r]);
    // A centred low end.
    let (l, r) = stereo_sine(60.0, 0.3, 0.0);
    let centred = t(3, None, vec![l, r]);
    let rep = run_with_master(&[wide, high, centred], None, SignalPoint::Processed);
    let w = of_rule(&rep, RuleId::WideLowEnd);
    assert_eq!(w.len(), 1, "{:#?}", rep.findings);
    let f = w[0];
    assert_eq!(f.subject, Subject::Track(TrackId(1)));
    assert!(
        (ev(f, labels::WIDTH) - -2.7).abs() < 0.3,
        "{}",
        ev(f, labels::WIDTH)
    );
    assert!(f.recommendations.iter().any(|x| matches!(
        x.experiment,
        Experiment::SideLowCut {
            track: TrackId(1),
            ..
        }
    )));
    assert!(of_rule(&rep, RuleId::MonoFoldDownLoss).is_empty());
    // Every stereo subject has its width per band and region.
    assert_eq!(rep.details.width.len(), 3);
    let hw = rep
        .details
        .width_of(Subject::Track(TrackId(2)))
        .map_or(f64::NAN, |w| w.broad_width_db[3]);
    assert!(hw.abs() < 0.5, "uncorrelated mids: {hw}");
}

#[test]
fn the_mix_s_balance_in_mono_and_who_makes_it() {
    // A centred bass four times the power of a wide top whose channels
    // correlate at −0.4: the top drops about 4.6 dB against the rest in
    // mono, without a 6 dB fold-down loss of its own.
    let (bl, br) = stereo_sine(100.0, 0.4, 0.0);
    let (hl, hr) = stereo_sine(4000.0, 0.2, (-0.4f64).acos());
    let mix = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(x, y)| x + y).collect() };
    let master = t(99, None, vec![mix(&bl, &hl), mix(&br, &hr)]);
    let tracks = [
        t(1, Some(TrackRole::Bass), vec![bl.clone(), br.clone()]),
        t(2, Some(TrackRole::Pads), vec![hl.clone(), hr.clone()]),
    ];
    let rep = run_with_master(&tracks, Some(&master), SignalPoint::Processed);
    let m = of_rule(&rep, RuleId::MonoBalanceShift);
    assert_eq!(m.len(), 1, "{:#?}", rep.findings);
    let f = m[0];
    assert_eq!(f.subject, Subject::Master(Some(TrackId(99))));
    assert!(
        (ev(f, labels::MONO_SHIFT) - -4.6).abs() < 0.4,
        "{}",
        ev(f, labels::MONO_SHIFT)
    );
    let range = f
        .frequency
        .unwrap_or(crate::FrequencyRange { lo: 0.0, hi: 0.0 });
    assert!(range.lo < 4000.0 && range.hi > 4000.0);
    // The pad brings the side signal there.
    assert!(
        f.evidence
            .iter()
            .any(|e| e.label == labels::SIDE_SHARE && e.track == Some(TrackId(2)) && e.value > 0.9)
    );
    assert!(f.tracks.contains(&TrackId(2)));
    // A normally wide top (uncorrelated): the usual 3 dB, no finding.
    let (hl, hr) = stereo_sine(4000.0, 0.2, std::f64::consts::FRAC_PI_2);
    let master = t(99, None, vec![mix(&bl, &hl), mix(&br, &hr)]);
    let rep = run_with_master(&[], Some(&master), SignalPoint::Processed);
    assert!(of_rule(&rep, RuleId::MonoBalanceShift).is_empty());
    assert!(rep.findings.is_empty(), "{:#?}", rep.findings);
}

/// Profiles of a source and of it processed, through the extra rules.
fn flatten_report(source: &[f32], processed: &[f32], sends: bool) -> Report {
    let rules = Rules::default();
    let (ps, pp) = (profile(&[source]), profile(&[processed, processed]));
    let input = DiagnoseInput {
        tracks: vec![TrackInput {
            track: TrackId(1),
            role: Some(TrackRole::Kick),
            profile: &pp,
            compare: true,
            focus: true,
        }],
        master: None,
        signal: SignalPoint::Processed,
        sections: Vec::new(),
        rules: &rules,
        cancel: None,
        related: Vec::new(),
    };
    let mut report = diagnose(&input).unwrap_or_default();
    let extra = ExtraInput {
        sources: vec![SourceInput {
            track: TrackId(1),
            profile: &ps,
            sends,
        }],
        ..ExtraInput::default()
    };
    diagnose_extra(&input, &extra, &mut report).unwrap_or_else(|_| panic!("cancelled"));
    report
}

#[test]
fn flattened_transients_are_possible_and_a_fader_is_not() {
    let x = hits(0.8, 0.5, LEN);
    let squashed = squash(&x, 0.05);
    let r = flatten_report(&x, &squashed, false);
    let f = of_rule(&r, RuleId::TransientFlattening);
    assert_eq!(f.len(), 1, "{:#?}", r.findings);
    let f = f[0];
    assert!(ev(f, labels::TSR_CHANGE) < -4.0);
    assert!(ev(f, labels::TSR_SOURCE) > 8.0);
    assert!(f.rule.name().contains("Possible"));
    assert!(
        f.recommendations
            .iter()
            .all(|x| !x.experiment.previewable()),
        "only by hand: the device is not identified"
    );
    let d = r
        .details
        .dynamics_of(Subject::Track(TrackId(1)))
        .and_then(|d| d.flattening);
    assert!(d.is_some_and(|d| d.tsr_change_db < -4.0));
    // With sends: the same measurement, less sure, said why.
    let s = flatten_report(&x, &squashed, true);
    let g = of_rule(&s, RuleId::TransientFlattening);
    assert_eq!(g.len(), 1);
    assert!(g[0].confidence < f.confidence);
    assert!(g[0].caveats.iter().any(|c| c.contains("sends")));
    // Only quieter (a fader): the shape is the same, nothing reported.
    let quieter: Vec<f32> = x.iter().map(|v| v * 0.3).collect();
    let r = flatten_report(&x, &quieter, false);
    assert!(of_rule(&r, RuleId::TransientFlattening).is_empty());
    // A steady tone has no transients to flatten.
    let s = tone(200.0, 0.3, 0.0, LEN, LEN);
    let r = flatten_report(&s, &squash(&s, 0.05), false);
    assert!(of_rule(&r, RuleId::TransientFlattening).is_empty());
}

#[test]
fn a_wide_loudness_range_and_a_level_jump_against_a_steady_mix() {
    let len = 60.0;
    // 30 s at −30 dBFS RMS, then 30 s at −10 (a 500 Hz tone).
    let a = tone(500.0, 0.0447, 0.0, 30.0, len);
    let b = tone(500.0, 0.447, 30.0, len, len);
    let track: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    // The mix: the track under a steady bed 6 dB over its loud part.
    let bed = tone(3000.0, 0.9, 0.0, len, len);
    let mix: Vec<f32> = track.iter().zip(&bed).map(|(x, y)| x + y).collect();
    let rep = run_with_master(
        &[t(1, None, vec![track.clone()])],
        Some(&t(99, None, vec![mix])),
        SignalPoint::Processed,
    );
    let w = of_rule(&rep, RuleId::WideLoudnessRange);
    assert_eq!(w.len(), 1, "{:#?}", rep.findings);
    assert!((ev(w[0], labels::LRA) - 20.0).abs() < 1.5);
    assert!(ev(w[0], labels::MIX_LRA) < 5.0);
    let j = of_rule(&rep, RuleId::LevelJump);
    assert_eq!(j.len(), 1, "{:#?}", rep.findings);
    let f = j[0];
    assert!((ev(f, labels::STEP) - 20.0).abs() < 1.0);
    assert!(ev(f, labels::MIX_STEP).abs() < 3.0);
    let at = f.time.map_or(0.0, |t| 0.5 * (t.start + t.end));
    assert!((at - 30.0).abs() < 0.5, "{at}");
    assert!(matches!(
        rep.details.chart(f.id),
        Some(crate::Chart::Time { .. })
    ));
    // The mix makes the same step (the arrangement): neither is reported.
    let rep = run_with_master(
        &[t(1, None, vec![track.clone()])],
        Some(&t(99, None, vec![track.clone()])),
        SignalPoint::Processed,
    );
    assert!(of_rule(&rep, RuleId::WideLoudnessRange).is_empty());
    assert!(of_rule(&rep, RuleId::LevelJump).is_empty());
    // A steady track: nothing.
    let steady = tone(500.0, 0.3, 0.0, len, len);
    let rep = run_with_master(&[t(1, None, vec![steady])], None, SignalPoint::Processed);
    assert!(rep.findings.is_empty(), "{:#?}", rep.findings);
    // The indicators are in the details for every subject.
    let d = rep
        .details
        .dynamics_of(Subject::Track(TrackId(1)))
        .map(|d| d.dynamics.clone());
    let d = d.unwrap_or_default();
    assert!((d.crest_db.unwrap_or(0.0) - 3.01).abs() < 0.05);
    assert!(d.plr_db.is_some() && d.lra_lu.is_some_and(|l| l < 0.5));
}

/// A sustained harmonic sound on `f0` (eight harmonics), `len` seconds.
fn pad(f0: f64, len: f64) -> Vec<f32> {
    let parts: Vec<Vec<f32>> = (1..=8)
        .map(|h| tone(f0 * h as f64, 0.2 * 0.7f64.powi(h - 1), 0.0, len, len))
        .collect();
    let refs: Vec<&[f32]> = parts.iter().map(Vec::as_slice).collect();
    crate::test_support::mix(&refs)
}

/// Diagnose two tracks (no roles) with `rules`.
fn diagnose_two(a: &T, b: &T, rules: &Rules) -> Report {
    let (pa, pb) = (prof(a), prof(b));
    let input = DiagnoseInput {
        tracks: vec![
            TrackInput {
                track: TrackId(a.id),
                role: a.role,
                profile: &pa,
                compare: true,
                focus: true,
            },
            TrackInput {
                track: TrackId(b.id),
                role: b.role,
                profile: &pb,
                compare: true,
                focus: true,
            },
        ],
        master: None,
        signal: SignalPoint::Processed,
        sections: Vec::new(),
        rules,
        cancel: None,
        related: Vec::new(),
    };
    diagnose(&input).unwrap_or_default()
}

#[test]
fn copies_are_not_listed_as_masking_each_other() {
    let x = pad(110.0, LEN);
    let z = vec![0.0f32; x.len()];
    let rules = Rules::default();
    let scaled = |g: f32| -> Vec<f32> { x.iter().map(|v| g * v).collect() };
    let mut n1 = Noise(91);
    let mut n2 = Noise(92);
    let copies: Vec<(&str, T)> = vec![
        // The same audio panned to the right.
        ("panned", t(2, None, vec![z.clone(), scaled(0.9)])),
        // At another gain (6 dB down: it would be the masked one).
        ("quieter", t(2, None, vec![scaled(0.5)])),
        // Through an EQ.
        (
            "equalised",
            t(2, None, vec![crate::same::tests::eq(&x, 800.0)]),
        ),
        // A few samples later.
        ("delayed", t(2, None, vec![delayed(&x, 4.0)])),
        // The right channel of a stereo recording whose left is track 1:
        // quieter, two samples later, a little of its own room.
        (
            "split stereo",
            t(
                2,
                None,
                vec![
                    delayed(&x, 2.0)
                        .iter()
                        .map(|v| 0.6 * v + (0.002 * n2.next()) as f32)
                        .collect(),
                ],
            ),
        ),
    ];
    let left: Vec<f32> = x.iter().map(|v| v + (0.002 * n1.next()) as f32).collect();
    for (what, copy) in &copies {
        let a = if *what == "split stereo" {
            t(1, None, vec![left.clone()])
        } else {
            t(1, None, vec![x.clone()])
        };
        let r = diagnose_two(&a, copy, &rules);
        assert!(
            of_rule(&r, RuleId::SpectralMasking).is_empty(),
            "{what}: {:#?}",
            of_rule(&r, RuleId::SpectralMasking)
        );
        let s = of_rule(&r, RuleId::SameAudio);
        assert_eq!(s.len(), 1, "{what}: {:#?}", r.findings);
        let f = s[0];
        assert_eq!(
            f.subject,
            Subject::Tracks {
                a: TrackId(1),
                b: TrackId(2)
            }
        );
        assert!(ev(f, labels::COHERENCE) > 0.9, "{what}: {f:#?}");
        assert!(ev(f, labels::SAME_SECONDS) > 9.0, "{what}");
        assert!(
            f.severity < 0.35,
            "information, not a fault: {}",
            f.severity
        );
        assert!(r.details.chart(f.id).is_some());
        // Its explanation is worded from the measurements.
        let text = f.explanation(&|t| format!("T{}", t.0));
        assert!(
            text.contains("same audio") && text.contains("not judged"),
            "{text}"
        );
    }
    // Two equal copies: +6 dB over one in mono, said so.
    let r = diagnose_two(
        &t(1, None, vec![x.clone()]),
        &t(2, None, vec![x.clone()]),
        &rules,
    );
    let f = of_rule(&r, RuleId::SameAudio)[0];
    assert!((ev(f, labels::MONO_SUM) - 6.02).abs() < 0.1);
    assert!(f.explanation(&|t| format!("T{}", t.0)).contains("+6.0 dB"));
    // The same pair with the same-audio judgement switched off: masking.
    let mut off = Rules::default();
    off.same.coherence = 2.0;
    let r = diagnose_two(
        &t(1, None, vec![x.clone()]),
        &t(2, None, vec![scaled(0.5)]),
        &off,
    );
    assert!(!of_rule(&r, RuleId::SpectralMasking).is_empty());
}

#[test]
fn a_pair_the_same_in_one_section_is_judged_per_section() {
    let x = pad(110.0, LEN);
    // Another performance from six seconds on: tuned a little apart.
    let other = pad(110.5, LEN);
    let half = (LEN / 2.0 * f64::from(SR)) as usize;
    let b: Vec<f32> = x[..half].iter().chain(&other[half..]).copied().collect();
    let r = diagnose_two(
        &t(1, None, vec![x.clone()]),
        &t(2, None, vec![b.clone()]),
        &Rules::default(),
    );
    let s = of_rule(&r, RuleId::SameAudio);
    assert_eq!(s.len(), 1, "{:#?}", r.findings);
    let span = s[0].time.unwrap_or(crate::TimeSpan::new(0.0, 0.0));
    assert!(span.start < 0.5 && (span.end - 6.0).abs() < 2.5, "{span:?}");
    // Masking is judged where they differ, and only there.
    let m = of_rule(&r, RuleId::SpectralMasking);
    assert!(!m.is_empty(), "{:#?}", r.findings);
    for f in &m {
        for e in &f.events {
            assert!(e.start >= 4.0, "{e:?}");
        }
        assert!(f.caveats.iter().any(|c| c.contains("same audio")));
    }
    // Without the judgement the first half is masking too.
    let mut off = Rules::default();
    off.same.coherence = 2.0;
    let r = diagnose_two(&t(1, None, vec![x]), &t(2, None, vec![b]), &off);
    let m = of_rule(&r, RuleId::SpectralMasking);
    assert!(m.iter().any(|f| f.events.iter().any(|e| e.start < 1.0)));
}

#[test]
fn two_performances_of_one_part_are_still_judged_for_masking() {
    // The same notes played twice: tuned a hair apart, each with its own
    // slow vibrato — the same part, different audio.
    let rate = f64::from(SR);
    let take = |f0: f64, rate_hz: f64| -> Vec<f32> {
        (0..(LEN * rate) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                let f = f0 * (1.0 + 0.003 * (TAU * rate_hz * t).sin());
                (1..=8)
                    .map(|h| 0.2 * 0.7f64.powi(h - 1) * (TAU * f * h as f64 * t).sin())
                    .sum::<f64>() as f32
            })
            .collect()
    };
    let r = diagnose_two(
        &t(1, Some(TrackRole::BackingVocals), vec![take(110.0, 5.0)]),
        &t(2, Some(TrackRole::BackingVocals), vec![take(110.2, 5.7)]),
        &Rules::default(),
    );
    assert!(
        of_rule(&r, RuleId::SameAudio).is_empty(),
        "{:#?}",
        r.findings
    );
    let m = of_rule(&r, RuleId::SpectralMasking);
    assert!(!m.is_empty(), "{:#?}", r.findings);
    assert!(
        m.iter()
            .all(|f| !f.caveats.iter().any(|c| c.contains("same audio")))
    );
}
