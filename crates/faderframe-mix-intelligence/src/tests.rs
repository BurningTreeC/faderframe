//! The rules on synthetic mixes with known relationships.

use crate::finding::{Experiment, RuleId, SignalPoint, Subject, TimeSpan};
use crate::rules::{DiagnoseInput, Report, TrackInput, diagnose, pair_weight};
use crate::test_support::{SR, mix, profile, profile_with_spans, tone};
use crate::{Category, Rules, TrackProfile};
use faderframe_core::{TrackId, TrackRole};
use std::f64::consts::TAU;

const LEN: f64 = 12.0;

/// Kick drums: a 0.25 s hit every half second, a falling 55 Hz sine.
fn kick(amp: f64) -> Vec<f32> {
    let rate = f64::from(SR);
    (0..(LEN * rate) as usize)
        .map(|i| {
            let t = i as f64 / rate;
            let k = t % 0.5;
            if k > 0.25 {
                return 0.0;
            }
            let f = 55.0 + 40.0 * (-k * 40.0).exp();
            (amp * (-k * 8.0).exp() * (TAU * f * k).sin()) as f32
        })
        .collect()
}

/// A sustained bass at `hz` with two harmonics.
fn bass(hz: f64, amp: f64) -> Vec<f32> {
    mix(&[
        &tone(hz, amp, 0.0, LEN, LEN),
        &tone(2.0 * hz, amp * 0.4, 0.0, LEN, LEN),
        &tone(3.0 * hz, amp * 0.2, 0.0, LEN, LEN),
    ])
}

/// A voice-like harmonic tone (220 Hz, harmonics to 3 kHz falling 3 dB
/// each).
fn voice(amp: f64) -> Vec<f32> {
    let parts: Vec<Vec<f32>> = (1..=13)
        .map(|h| tone(220.0 * h as f64, amp * 0.7f64.powi(h - 1), 0.0, LEN, LEN))
        .collect();
    let refs: Vec<&[f32]> = parts.iter().map(Vec::as_slice).collect();
    mix(&refs)
}

/// Keys: a chord's harmonics over the voice's range.
fn keys(amp: f64) -> Vec<f32> {
    let mut parts = Vec::new();
    for root in [261.6, 329.6, 392.0] {
        for h in 1..=6 {
            parts.push(tone(
                root * h as f64,
                amp * 0.6f64.powi(h - 1),
                0.0,
                LEN,
                LEN,
            ));
        }
    }
    let refs: Vec<&[f32]> = parts.iter().map(Vec::as_slice).collect();
    mix(&refs)
}

fn input<'a>(
    tracks: &'a [(u64, Option<TrackRole>, &'a TrackProfile)],
    master: Option<&'a TrackProfile>,
    rules: &'a Rules,
) -> DiagnoseInput<'a> {
    DiagnoseInput {
        tracks: tracks
            .iter()
            .map(|(id, role, p)| TrackInput {
                track: TrackId(*id),
                role: *role,
                profile: p,
                compare: true,
                focus: true,
            })
            .collect(),
        master: master.map(|p| (Some(TrackId(99)), p)),
        signal: SignalPoint::Processed,
        sections: Vec::new(),
        rules,
        cancel: None,
    }
}

fn run(tracks: &[(u64, Option<TrackRole>, &TrackProfile)]) -> Report {
    let rules = Rules::default();
    diagnose(&input(tracks, None, &rules)).unwrap_or_default()
}

fn masking(r: &Report) -> Vec<&crate::Finding> {
    r.findings
        .iter()
        .filter(|f| f.rule == RuleId::SpectralMasking)
        .collect()
}

#[test]
fn kick_and_bass_conflict_low_down_and_the_experiments_say_so() {
    let k = profile(&[&kick(0.8)]);
    let b = profile(&[&bass(62.0, 0.35)]);
    let r = run(&[
        (1, Some(TrackRole::Kick), &k),
        (2, Some(TrackRole::Bass), &b),
    ]);
    let m = masking(&r);
    assert_eq!(m.len(), 1, "one direction per range: {m:#?}");
    let f = m[0];
    let range = f
        .frequency
        .unwrap_or(crate::FrequencyRange { lo: 0.0, hi: 0.0 });
    assert!(range.lo < 62.0 && range.hi > 55.0, "{range:?}");
    assert!(f.tracks.contains(&TrackId(1)) && f.tracks.contains(&TrackId(2)));
    // Roles known: full role weight, both directions considered.
    assert_eq!(f.severity_terms[0].value, 1.0);
    assert!(f.confidence > 0.6, "{}", f.confidence);
    assert!(f.time.is_some() && !f.events.is_empty());
    // The experiments: on the masker, keyed by the target, the target's
    // level, and the kick/bass phase check (by hand).
    let Subject::Pair { target, masker } = f.subject else {
        panic!("{:?}", f.subject);
    };
    let kinds: Vec<&Experiment> = f.recommendations.iter().map(|r| &r.experiment).collect();
    assert!(
        kinds
            .iter()
            .any(|e| matches!(e, Experiment::EqCut { track, .. } if *track == masker))
    );
    assert!(kinds.iter().any(
        |e| matches!(e, Experiment::DynamicEqCut { track, key, .. } if *track == masker && *key == target)
    ));
    assert!(
        kinds
            .iter()
            .any(|e| matches!(e, Experiment::Level { track, .. } if *track == target))
    );
    assert!(kinds.iter().any(|e| matches!(e, Experiment::Manual { .. })));
    for r in &f.recommendations {
        assert!(
            r.experiment.validate(24_000.0).is_ok(),
            "{:?}",
            r.experiment
        );
        // Every experiment's frequency is inside the finding's range.
        if let Experiment::EqCut { freq_hz, .. } = r.experiment {
            assert!(freq_hz >= range.lo && freq_hz <= range.hi);
        }
    }
    let name = |t: TrackId| {
        if t.0 == 1 {
            "Kick".into()
        } else {
            "Bass".into()
        }
    };
    let title = f.title(&name);
    assert!(title.contains("may mask"), "{title}");
    assert!(f.explanation(&name).contains("potential masking conflict"));
    // Without roles: neutral weight, less confident, a caveat about it.
    let neutral = run(&[(1, None, &k), (2, None, &b)]);
    let n = masking(&neutral);
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].severity_terms[0].value, 0.8);
    assert!(n[0].confidence < f.confidence);
    assert!(n[0].caveats.iter().any(|c| c.contains("roles")));
}

#[test]
fn a_lead_vocal_is_the_one_masked_by_its_accompaniment() {
    let v = profile(&[&voice(0.2)]);
    let k = profile(&[&keys(0.2)]);
    let r = run(&[
        (1, Some(TrackRole::LeadVocal), &v),
        (2, Some(TrackRole::Keys), &k),
    ]);
    let m = masking(&r);
    assert!(!m.is_empty(), "{:#?}", r.findings);
    for f in &m {
        assert_eq!(
            f.subject,
            Subject::Pair {
                target: TrackId(1),
                masker: TrackId(2)
            }
        );
        assert!(f.frequency.is_some_and(|x| x.lo >= 150.0));
    }
}

#[test]
fn rhythm_guitar_against_bass_and_two_related_instruments() {
    // A "guitar" whose lowest notes reach the bass's range.
    let g = profile(&[&bass(98.0, 0.3)]);
    let b = profile(&[&bass(98.0, 0.3)]);
    let r = run(&[
        (1, Some(TrackRole::RhythmGuitar), &g),
        (2, Some(TrackRole::Bass), &b),
    ]);
    let m = masking(&r);
    assert!(!m.is_empty());
    // The bass as the target weighs more (0.9) than the guitar (0.85).
    assert_eq!(
        m[0].subject,
        Subject::Pair {
            target: TrackId(2),
            masker: TrackId(1)
        }
    );
    // Two accompanying instruments (keys and keys) overlap too.
    let a = profile(&[&keys(0.2)]);
    let c = profile(&[&keys(0.18)]);
    let r = run(&[
        (1, Some(TrackRole::Keys), &a),
        (2, Some(TrackRole::Pads), &c),
    ]);
    assert!(!masking(&r).is_empty());
}

#[test]
fn one_drum_kit_and_separate_material_are_not_masking() {
    let k = profile(&[&kick(0.8)]);
    let r = run(&[
        (1, Some(TrackRole::Kick), &k),
        (2, Some(TrackRole::Drums), &k),
    ]);
    assert!(masking(&r).is_empty(), "microphones of one kit");
    assert_eq!(
        pair_weight(Some(TrackRole::Snare), Some(TrackRole::Kick)),
        None
    );
    // Apart in frequency, apart in time, far apart in level.
    let low = profile(&[&tone(70.0, 0.3, 0.0, LEN, LEN)]);
    let high = profile(&[&tone(4000.0, 0.3, 0.0, LEN, LEN)]);
    assert!(masking(&run(&[(1, None, &low), (2, None, &high)])).is_empty());
    let early = profile(&[&tone(70.0, 0.3, 0.0, 5.0, LEN)]);
    let late = profile(&[&tone(74.0, 0.3, 6.0, LEN, LEN)]);
    assert!(masking(&run(&[(1, None, &early), (2, None, &late)])).is_empty());
    // Far apart in level: the quiet one may be masked, never the loud one.
    let quiet = profile(&[&tone(74.0, 0.01, 0.0, LEN, LEN)]);
    let r = run(&[(1, None, &low), (2, None, &quiet)]);
    let m = masking(&r);
    assert_eq!(m.len(), 1);
    assert_eq!(
        m[0].subject,
        Subject::Pair {
            target: TrackId(2),
            masker: TrackId(1)
        }
    );
    assert!(
        m[0].evidence
            .iter()
            .any(|e| e.label == crate::finding::labels::MARGIN && e.value > 25.0)
    );
    let silent = profile(&[&vec![0.0f32; (LEN * f64::from(SR)) as usize]]);
    let r = run(&[(1, None, &silent), (2, None, &silent)]);
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
}

#[test]
fn severity_follows_persistence_and_findings_are_ranked() {
    // The masker 9.5 dB over the target (never masked by it in turn):
    // all the time, or for 2.5 s.
    // (Both inside the 80 Hz third octave: a tone near a band's edge
    // spreads into the next band at this resolution.)
    let target = profile(&[&tone(80.0, 0.1, 0.0, LEN, LEN)]);
    let always = profile(&[&tone(84.0, 0.3, 0.0, LEN, LEN)]);
    let briefly = profile(&[&tone(84.0, 0.3, 2.0, 4.5, LEN)]);
    let a = run(&[(1, None, &target), (2, None, &always)]);
    let b = run(&[(1, None, &target), (3, None, &briefly)]);
    let (fa, fb) = (masking(&a), masking(&b));
    assert!(!fa.is_empty() && !fb.is_empty());
    assert!(
        fa[0].severity > fb[0].severity + 0.2,
        "{} {}",
        fa[0].severity,
        fb[0].severity
    );
    assert_eq!(fa[0].pattern, Some(crate::Pattern::Persistent));
    assert_eq!(fb[0].pattern, Some(crate::Pattern::Isolated), "{:#?}", fb);
    // Located: the event is the burst.
    let e = fb[0].events[0];
    assert!(
        (e.start - 2.0).abs() < 0.25 && (e.end - 4.5).abs() < 0.25,
        "{e:?}"
    );
    assert_eq!(fb[0].time, Some(e));
    // Both together: ranked by relevance, deterministic.
    let all = run(&[(1, None, &target), (2, None, &always), (3, None, &briefly)]);
    for w in all.findings.windows(2) {
        assert!(w[0].relevance() >= w[1].relevance());
    }
    assert_eq!(
        all,
        run(&[(1, None, &target), (2, None, &always), (3, None, &briefly)])
    );
}

#[test]
fn the_master_levels_are_checked_against_the_rules() {
    let rules = Rules::default();
    // A hot master: a 100 Hz sine clipped at full scale.
    let hot: Vec<f32> = tone(100.0, 1.6, 0.0, LEN, LEN)
        .into_iter()
        .map(|v| v.clamp(-1.0, 1.0))
        .collect();
    let m = profile(&[&hot, &hot]);
    let r = diagnose(&input(&[], Some(&m), &rules)).unwrap_or_default();
    let tp = r
        .findings
        .iter()
        .find(|f| f.rule == RuleId::MasterTruePeak)
        .unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert!(matches!(
        tp.recommendations[0].experiment,
        Experiment::Level { track: TrackId(99), change_db } if change_db < 0.0
    ));
    assert!(r.findings.iter().any(|f| f.rule == RuleId::ClippedSamples));
    // A master at −6 dBFS: nothing to report.
    let fine = profile(&[&tone(100.0, 0.5, 0.0, LEN, LEN)]);
    let r = diagnose(&input(&[], Some(&fine), &rules)).unwrap_or_default();
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
    assert_eq!(r.summaries.len(), 1);
    let s = &r.summaries[0];
    assert!((s.crest - 3.01).abs() < 0.05, "a sine's crest: {}", s.crest);
    assert!(s.integrated.is_some());
    // Sections 12 LU apart.
    let quiet_loud = mix(&[
        &tone(1000.0, 0.05, 0.0, 6.0, LEN),
        &tone(1000.0, 0.2, 6.0, LEN, LEN),
    ]);
    let half = 6 * SR as usize;
    let spans = [(0, half), (half, 2 * half)];
    let p = profile_with_spans(&[&quiet_loud, &quiet_loud], &spans);
    let mut i = input(&[], Some(&p), &rules);
    i.sections = vec![
        ("Verse".into(), TimeSpan::new(0.0, 6.0)),
        ("Chorus".into(), TimeSpan::new(6.0, 12.0)),
    ];
    let r = diagnose(&i).unwrap_or_default();
    let step = r
        .findings
        .iter()
        .find(|f| f.rule == RuleId::SectionLoudnessStep)
        .unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!(step.sections, ["Verse", "Chorus"]);
    assert!(step.severity < 0.25, "often intended: low");
    assert_eq!(r.summaries[0].sections.len(), 2);
}

#[test]
fn a_track_over_full_scale_and_invalid_samples() {
    let rules = Rules::default();
    let over = profile(&[&tone(200.0, 1.4, 0.0, LEN, LEN)]);
    let mut broken = tone(200.0, 0.3, 0.0, LEN, LEN);
    broken[1000] = f32::NAN;
    broken[2000] = f32::INFINITY;
    let broken = profile(&[&broken]);
    let r = diagnose(&input(
        &[(1, None, &over), (2, None, &broken)],
        None,
        &rules,
    ))
    .unwrap_or_default();
    let of = r
        .findings
        .iter()
        .find(|f| f.rule == RuleId::OverFullScale)
        .unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!(of.subject, Subject::Track(TrackId(1)));
    assert!(matches!(
        of.recommendations[0].experiment,
        Experiment::Level { change_db, .. } if (-4.0..0.0).contains(&change_db)
    ));
    let nf = r
        .findings
        .iter()
        .find(|f| f.rule == RuleId::NonFiniteSamples)
        .unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!(nf.evidence[0].value, 2.0);
    assert!(r.findings.iter().all(|f| f.severity.is_finite()));
}

#[test]
fn stereo_rules_tell_cancellation_from_width() {
    let rules = Rules::default();
    let s = tone(1000.0, 0.3, 0.0, LEN, LEN);
    let inv: Vec<f32> = s.iter().map(|v| -v).collect();
    let inverted = profile(&[&s, &inv]);
    // A wide low end: 60 Hz with the right channel 90° behind (−3 dB in
    // mono; the low-end threshold).
    let rate = f64::from(SR);
    let l60: Vec<f32> = (0..(LEN * rate) as usize)
        .map(|i| (0.3 * (TAU * 60.0 * i as f64 / rate).sin()) as f32)
        .collect();
    let r60: Vec<f32> = (0..(LEN * rate) as usize)
        .map(|i| (0.3 * (TAU * 60.0 * i as f64 / rate - 2.4).sin()) as f32)
        .collect();
    let wide_low = profile(&[&l60, &r60]);
    // A wide pad high up: uncorrelated (−3 dB in mono) is width, not a
    // fault.
    let r1k: Vec<f32> = (0..(LEN * rate) as usize)
        .map(|i| {
            (0.3 * (TAU * 1000.0 * i as f64 / rate - std::f64::consts::FRAC_PI_2).sin()) as f32
        })
        .collect();
    let wide_high = profile(&[&s, &r1k]);
    let centred = profile(&[&s, &s]);
    let r = diagnose(&input(
        &[
            (1, None, &inverted),
            (2, None, &wide_low),
            (3, None, &wide_high),
            (4, None, &centred),
        ],
        None,
        &rules,
    ))
    .unwrap_or_default();
    let of = |t: u64, rule: RuleId| {
        r.findings
            .iter()
            .filter(|f| f.rule == rule && f.subject == Subject::Track(TrackId(t)))
            .count()
    };
    assert_eq!(of(1, RuleId::InvertedChannel), 1);
    assert_eq!(of(1, RuleId::MonoFoldDownLoss), 1);
    assert_eq!(of(2, RuleId::MonoFoldDownLoss), 1, "{:#?}", r.findings);
    assert_eq!(of(2, RuleId::InvertedChannel), 1, "−0.74 correlation");
    assert_eq!(of(3, RuleId::MonoFoldDownLoss), 0, "width is no fault");
    assert_eq!(of(3, RuleId::InvertedChannel), 0);
    assert_eq!(
        of(4, RuleId::MonoFoldDownLoss) + of(4, RuleId::InvertedChannel),
        0
    );
    // The low end's experiment: mono below the range.
    let low = r
        .findings
        .iter()
        .find(|f| f.rule == RuleId::MonoFoldDownLoss && f.subject == Subject::Track(TrackId(2)))
        .unwrap_or_else(|| panic!());
    assert!(low.recommendations.iter().any(|x| matches!(
        x.experiment,
        Experiment::SideLowCut {
            track: TrackId(2),
            ..
        }
    )));
    assert!(low.category() == Category::Stereo);
}

#[test]
fn cancelling_stops_the_diagnosis() {
    let rules = Rules::default();
    let a = profile(&[&tone(70.0, 0.3, 0.0, LEN, LEN)]);
    let stop = std::sync::atomic::AtomicBool::new(true);
    let tracks = [(1, None, &a), (2, None, &a)];
    let mut i = input(&tracks, None, &rules);
    i.cancel = Some(&stop);
    assert!(diagnose(&i).is_err());
}
