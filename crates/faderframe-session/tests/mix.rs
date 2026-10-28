//! Mix Intelligence in a session: analyses rendered offline and taken in
//! from the tick, cached, superseded, cancelled and found stale; findings
//! shown on the arranger; experiments auditioned in the engine, rejected
//! (the project as it was), applied as one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{TrackId, TrackRole};
use faderframe_engine::EngineConfig;
use faderframe_mix_intelligence::{Experiment, RuleId, SignalPoint, Subject};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Project, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::mix::{MixOp, MixScope, MixStatus};
use faderframe_session::{Action, SelectMode, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

/// A sine track: `hz` at `amp` from `a` to `b` seconds (generators play
/// at a peak of 0.7: the clip's gain sets the amplitude).
fn sine(p: &mut Project, name: &str, hz: f64, amp: f32, a: f64, b: f64) -> TrackId {
    let src = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: name.into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Sine {
                    frequency: hz,
                    seconds: b,
                    amplitude: amp,
                },
            },
        },
    );
    let id: TrackId = p.ids.allocate();
    let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(1));
    let rate = f64::from(SR);
    let clip = Clip {
        id: p.ids.allocate(),
        track: id,
        name: name.into(),
        color: None,
        start: p.timeline.to_musical((a * rate) as i64, rate),
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source: src,
            source_offset: (a * rate) as i64,
            length: ((b - a) * rate) as i64,
            gain_db: 20.0 * (amp / 0.7).log10(),
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
        }),
    };
    t.clips.push(clip.id);
    p.clips.insert(clip.id, clip);
    p.tracks.push(t);
    id
}

/// Two low tones in one third octave all the time, and a high one.
fn session() -> (Session, [TrackId; 3]) {
    let mut p = Project::new("Mix", SR);
    let a = sine(&mut p, "Low A", 80.0, 0.3, 0.0, 8.0);
    let b = sine(&mut p, "Low B", 84.0, 0.3, 0.0, 8.0);
    let h = sine(&mut p, "High", 5000.0, 0.2, 0.0, 8.0);
    let s = Session::new(p, None, EngineConfig::default()).unwrap();
    (s, [a, b, h])
}

fn analyse(s: &mut Session, scope: MixScope, signal: SignalPoint) {
    s.dispatch(Action::Mix(MixOp::Analyse { scope, signal }))
        .unwrap();
    assert!(s.mix_analysing());
    s.wait_for_mix();
    assert!(!s.mix_analysing());
}

fn masking(s: &Session) -> Vec<(TrackId, TrackId)> {
    s.mix_results()
        .unwrap()
        .report
        .findings
        .iter()
        .filter_map(|f| match f.subject {
            Subject::Pair { target, masker } => Some((target, masker)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_overlapping_tracks_are_found_and_shown() {
    let (mut s, [a, b, h]) = session();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    assert_eq!(s.mix_status(), MixStatus::Idle);
    let r = s.mix_results().unwrap();
    assert_eq!(r.signal, SignalPoint::Processed);
    assert_eq!(r.tracks.len(), 3);
    assert_eq!(
        (r.rendered, r.reused),
        (4, 0),
        "three tracks and the master"
    );
    assert!((r.seconds() - 8.0).abs() < 0.01, "{}", r.seconds());
    let pairs = masking(&s);
    assert!(!pairs.is_empty());
    for (t, m) in &pairs {
        assert!([a, b].contains(t) && [a, b].contains(m), "{pairs:?}");
        assert!(*t != h && *m != h);
    }
    // The master was measured (levels, no faults in a quiet mix).
    let r = s.mix_results().unwrap();
    assert!(
        r.report
            .summaries
            .iter()
            .any(|x| matches!(x.subject, Subject::Master(_)) && x.integrated.is_some())
    );
    // Showing a finding selects its tracks and its time.
    let f = s.mix_selected().unwrap().clone();
    s.dispatch(Action::Mix(MixOp::Show(f.id))).unwrap();
    let selected: Vec<TrackId> = s.selection.tracks.iter().copied().collect();
    assert!(selected.contains(&a) && selected.contains(&b));
    let range = s.selection.range.unwrap();
    assert!(
        range.start.quarters() < 1.0 && range.end.quarters() > 14.0,
        "{range:?}"
    );
    // Before processing too.
    analyse(&mut s, MixScope::Project, SignalPoint::Source);
    assert_eq!(s.mix_results().unwrap().signal, SignalPoint::Source);
    assert!(!masking(&s).is_empty());
    assert_eq!(
        s.mix_results().unwrap().rendered,
        3,
        "no master in source mode"
    );
}

#[test]
fn profiles_are_reused_until_the_sound_changes() {
    let (mut s, [a, b, _]) = session();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    // Renaming changes nothing heard: still fresh, all reused.
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: a,
        name: "Kick".into(),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackRole {
        track: a,
        role: Some(TrackRole::Kick),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackRole {
        track: b,
        role: Some(TrackRole::Bass),
    }))
    .unwrap();
    s.tick(0.016);
    assert_eq!(s.mix_stale(), None);
    let f = s.mix_selected().unwrap();
    assert!(
        f.title(&|t| s.mix_track_name(t)).contains("Kick"),
        "names now"
    );
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!((r.rendered, r.reused), (0, 4));
    // The roles weigh in now.
    let f = s.mix_selected().unwrap();
    assert_eq!(f.severity_terms[0].value, 1.0, "kick and bass");
    // A fader moves: stale where it reaches (that track's findings, the
    // master); one render measures that track and the master again, the
    // others' profiles are reused (nothing downstream changes a tap).
    let a_only = s
        .mix_findings()
        .iter()
        .find(|f| f.tracks == [a])
        .map(|f| f.id);
    s.dispatch(Action::Edit(Command::SetTrackVolume { track: b, db: -3.0 }))
        .unwrap();
    s.tick(0.016);
    assert!(s.mix_stale().is_some());
    let f = s.mix_selected().unwrap().id;
    assert!(s.mix_finding_stale(f), "the pair names the changed track");
    if let Some(id) = a_only {
        assert!(
            !s.mix_finding_stale(id),
            "a finding on the other track stays"
        );
    }
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!((r.rendered, r.reused, r.renders), (2, 2, 1));
    assert_eq!(s.mix_stale(), None);
    // Sources: the fader is not part of them…
    analyse(&mut s, MixScope::Project, SignalPoint::Source);
    s.dispatch(Action::Edit(Command::SetTrackVolume { track: b, db: -6.0 }))
        .unwrap();
    analyse(&mut s, MixScope::Project, SignalPoint::Source);
    assert_eq!(s.mix_results().unwrap().reused, 3);
    // …a clip's gain is, of that track only.
    let clip = s.project().track(a).unwrap().clips[0];
    let mut c = s.project().clip(clip).unwrap().clone();
    if let ClipContent::Audio(x) = &mut c.content {
        x.gain_db = -2.0;
    }
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(c.content),
    }))
    .unwrap();
    analyse(&mut s, MixScope::Project, SignalPoint::Source);
    let r = s.mix_results().unwrap();
    assert_eq!((r.rendered, r.reused), (1, 2));
}

#[test]
fn stale_superseded_cancelled_and_closed_analyses() {
    let (mut s, [a, b, h]) = session();
    // The project changes while it runs: the results come, marked stale.
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackVolume { track: h, db: -1.0 }))
        .unwrap();
    s.wait_for_mix();
    assert!(s.mix_results().is_some());
    assert!(s.mix_stale().is_some());
    // A newer analysis replaces the running one: only its results land.
    s.dispatch(Action::SelectTracks {
        tracks: vec![a],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Source,
    }))
    .unwrap();
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::SelectedTracks,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    let r = s.mix_results().unwrap();
    assert_eq!(
        (r.scope, r.signal),
        (MixScope::SelectedTracks, SignalPoint::Processed)
    );
    assert_eq!(r.focus, [a]);
    for f in &r.report.findings {
        assert!(
            f.tracks.is_empty() || f.tracks.contains(&a) || matches!(f.subject, Subject::Master(_))
        );
    }
    // A track deleted while it runs: stale, its experiments refused.
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::RemoveTrack { track: b }))
        .unwrap();
    s.wait_for_mix();
    assert!(s.mix_stale().unwrap().contains("deleted"));
    let f = s
        .mix_findings()
        .into_iter()
        .find(|f| f.tracks.contains(&b))
        .map(|f| f.id)
        .unwrap();
    assert!(s.mix_can_try(f, 0).is_err());
    assert_eq!(s.mix_track_name(b), "(deleted track)");
    // Cancelled: nothing new lands.
    let before = s.mix_results().unwrap().history_revision;
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Source,
    }))
    .unwrap();
    s.dispatch(Action::Mix(MixOp::Cancel)).unwrap();
    s.wait_for_mix();
    assert_eq!(s.mix_status(), MixStatus::Cancelled);
    assert_eq!(s.mix_results().unwrap().history_revision, before);
    // Another project: the old one's analysis never lands in it.
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.new_project(false).unwrap();
    s.wait_for_mix();
    assert!(s.mix_results().is_none());
    assert!(!s.mix_analysing());
}

fn eq_count(s: &Session, t: TrackId) -> usize {
    s.project()
        .track(t)
        .unwrap()
        .inserts
        .iter()
        .filter(|x| x.plugin.id == faderframe_core::builtin::EQ)
        .count()
}

#[test]
fn experiments_are_auditioned_rejected_applied_and_undone() {
    let (mut s, [_, _, h]) = session();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let f = s
        .mix_findings()
        .into_iter()
        .find(|f| f.rule == RuleId::SpectralMasking)
        .unwrap()
        .clone();
    let (i, masker) = f
        .recommendations
        .iter()
        .enumerate()
        .find_map(|(i, r)| match r.experiment {
            Experiment::EqCut { track, .. } => Some((i, track)),
            _ => None,
        })
        .unwrap();
    let tracks = s.project().tracks.clone();
    let steps = s.history_steps().0.len();
    // Audition: the engine plays it; no undo step; the project changed
    // only by the candidate.
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps);
    assert_eq!(eq_count(&s, masker), 1);
    // Loudness-matched: measured offline, the candidate (a cut) quieter,
    // played louder on the monitor by as much.
    s.wait_for_mix_measure();
    let a = s.mix_audition().unwrap();
    let m = a.measured.clone().unwrap().unwrap();
    assert!(m.change() < 0.0 && m.change() > -3.0, "{m:?}");
    assert!(m.band_change_db.unwrap() < -0.5, "{m:?}");
    assert!((a.trim_db + m.change()).abs() < 1e-9);
    // A/B: the original has no trim and the band bypassed.
    s.dispatch(Action::Mix(MixOp::Compare(false))).unwrap();
    let a = s.mix_audition().unwrap();
    assert!(!a.candidate && a.trim_db == 0.0);
    s.dispatch(Action::Mix(MixOp::LoudnessMatch(false)))
        .unwrap();
    s.dispatch(Action::Mix(MixOp::Compare(true))).unwrap();
    assert_eq!(s.mix_audition().unwrap().trim_db, 0.0, "matching off");
    // Rejected: the project as it was.
    s.dispatch(Action::Mix(MixOp::Reject)).unwrap();
    assert!(s.mix_audition().is_none());
    assert_eq!(s.project().tracks, tracks);
    // Something else edited while auditioning: the candidate goes first,
    // the edit is the only step, its undo brings no candidate back.
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: h,
        name: "Hat".into(),
    }))
    .unwrap();
    assert!(s.mix_audition().is_none());
    assert_eq!(eq_count(&s, masker), 0);
    assert_eq!(s.history_steps().0.len(), steps + 1);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks, tracks);
    // Applied (after an audition): one step, the EQ with its band.
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert!(s.mix_audition().is_none());
    let (done, _) = s.history_steps();
    assert_eq!(done.len(), steps + 1);
    assert!(
        done.last()
            .unwrap()
            .starts_with("Mix Intelligence: EQ cut on")
    );
    assert_eq!(eq_count(&s, masker), 1);
    assert!(s.mix_applied(f.id, i));
    let eq = s
        .project()
        .track(masker)
        .unwrap()
        .inserts
        .iter()
        .find(|x| x.plugin.id == faderframe_core::builtin::EQ)
        .unwrap()
        .clone();
    let gain = eq
        .parameters
        .iter()
        .find(|p| {
            p.id == faderframe_plugin_host::eq::band_id(0, faderframe_plugin_host::eq::Field::Gain)
        })
        .unwrap()
        .value;
    assert!((-4.0..0.0).contains(&gain), "{gain}");
    // The results are stale now: their experiments wait for a new analysis.
    s.tick(0.016);
    assert!(s.mix_stale().is_some());
    assert!(s.mix_can_try(f.id, i).is_err());
    // Undo: as it was.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks, tracks);
}

#[test]
fn experiments_that_cannot_be_tried_change_nothing() {
    let (mut s, [a, b, _]) = session();
    // Both low tracks' faders are automated (Read): level experiments on
    // them are refused.
    for t in [a, b] {
        let lane = faderframe_automation::AutomationLane {
            id: faderframe_core::AutomationLaneId(9_000 + t.0),
            target: faderframe_automation::AutomationTarget::TrackVolume,
            curve: faderframe_automation::AutomationCurve::from_points(vec![
                faderframe_automation::AutomationPoint {
                    time: MusicalTime::ZERO,
                    value: 0.7,
                    shape: faderframe_automation::CurveShape::Linear,
                },
            ]),
            mode: faderframe_automation::AutomationMode::Read,
            visible: true,
        };
        s.dispatch(Action::Edit(Command::AddAutomationLane {
            track: t,
            lane: Box::new(lane),
        }))
        .unwrap();
    }
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let f = s
        .mix_findings()
        .into_iter()
        .find(|f| f.rule == RuleId::SpectralMasking)
        .unwrap()
        .clone();
    let tracks = s.project().tracks.clone();
    let level = f
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::Level { .. }))
        .unwrap();
    let why = s.mix_can_try(f.id, level).unwrap_err();
    assert!(why.contains("automated"), "{why}");
    assert!(
        s.dispatch(Action::Mix(MixOp::Audition {
            finding: f.id,
            recommendation: level,
        }))
        .is_err()
    );
    // A manual suggestion, a missing finding or experiment: errors.
    if let Some(m) = f
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::Manual { .. }))
    {
        assert!(
            s.dispatch(Action::Mix(MixOp::Audition {
                finding: f.id,
                recommendation: m,
            }))
            .is_err()
        );
    }
    assert!(
        s.dispatch(Action::Mix(MixOp::Apply {
            finding: 42,
            recommendation: 0,
        }))
        .is_err()
    );
    assert!(
        s.dispatch(Action::Mix(MixOp::Audition {
            finding: f.id,
            recommendation: 99,
        }))
        .is_err()
    );
    // Nothing changed by any of it.
    assert_eq!(s.project().tracks, tracks);
    assert!(s.mix_audition().is_none());
    // Nothing to analyse: errors, not panics.
    assert!(
        s.dispatch(Action::Mix(MixOp::Analyse {
            scope: MixScope::Selection,
            signal: SignalPoint::Processed,
        }))
        .is_err(),
        "no range selected"
    );
    let mut empty = Session::new(Project::new("Empty", SR), None, EngineConfig::default()).unwrap();
    assert!(
        empty
            .dispatch(Action::Mix(MixOp::Analyse {
                scope: MixScope::Project,
                signal: SignalPoint::Source,
            }))
            .is_err()
    );
}

#[test]
fn a_missing_file_analyses_as_silence() {
    let mut p = Project::new("Missing", SR);
    let a = sine(&mut p, "Low A", 80.0, 0.3, 0.0, 6.0);
    let gone = sine(&mut p, "Gone", 84.0, 0.3, 0.0, 6.0);
    // Its source is a file that is not there.
    let src = match &p.clips[&p.track(gone).unwrap().clips[0]].content {
        ClipContent::Audio(x) => x.source,
        _ => unreachable!(),
    };
    p.sources.get_mut(&src).unwrap().spec = SourceSpec::File {
        path: std::env::temp_dir().join("ff-mix-no-such-file.wav"),
        channels: 1,
        frames: 6 * i64::from(SR),
        sample_rate: SR,
    };
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert!(r.tracks.contains(&a) && r.tracks.contains(&gone));
    assert!(masking(&s).is_empty(), "silence masks nothing");
    let silent = r
        .report
        .summaries
        .iter()
        .find(|x| x.subject == Subject::Track(gone))
        .unwrap();
    assert_eq!(silent.sample_peak, f64::NEG_INFINITY);
}

/// The demo (drums, bass, an instrument with MIDI effects, buses,
/// returns, modulators) analyses both ways over a few bars: every number
/// finite, a dynamic experiment (keyed from another track) auditioned and
/// taken out again.
#[test]
fn the_demo_analyses_as_mixed_and_as_recorded() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bar = |n: i32| s.project().timeline.meter.bar_start(n);
    let (a, b) = (bar(4), bar(6));
    s.dispatch(Action::SetEditRange(Some(
        faderframe_session::EditRange::new(a, b),
    )))
    .unwrap();
    for signal in [SignalPoint::Processed, SignalPoint::Source] {
        analyse(&mut s, MixScope::Selection, signal);
        let r = s.mix_results().unwrap();
        assert!(r.tracks.len() >= 3, "{:?}", r.tracks);
        for f in &r.report.findings {
            assert!(f.severity.is_finite() && f.confidence.is_finite());
            assert!((0.0..=1.0).contains(&f.severity) && (0.0..=1.0).contains(&f.confidence));
            assert!(f.evidence.iter().all(|e| e.value.is_finite()));
            for t in &f.tracks {
                assert!(s.project().track(*t).is_some());
            }
            if let Some(t) = f.time {
                assert!(t.start >= 0.0 && t.end <= r.seconds() + 0.2, "{t:?}");
            }
        }
        for x in &r.report.summaries {
            assert!(!x.rms.is_nan() && !x.correlation.is_nan());
        }
    }
    // A dynamic EQ keyed from the target, if the demo has a conflict.
    analyse(&mut s, MixScope::Selection, SignalPoint::Processed);
    let dynamic = s.mix_findings().into_iter().find_map(|f| {
        f.recommendations
            .iter()
            .position(|r| matches!(r.experiment, Experiment::DynamicEqCut { .. }))
            .map(|i| (f.id, i, f.recommendations[i].experiment.clone()))
    });
    if let Some((id, i, Experiment::DynamicEqCut { track, key, .. })) = dynamic {
        let tracks = s.project().tracks.clone();
        s.dispatch(Action::Mix(MixOp::Audition {
            finding: id,
            recommendation: i,
        }))
        .unwrap();
        let eq = s
            .project()
            .track(track)
            .unwrap()
            .inserts
            .last()
            .unwrap()
            .clone();
        assert_eq!(eq.sidechain, Some(key), "keyed from the target");
        s.wait_for_mix_measure();
        assert!(s.mix_audition().unwrap().measured.is_some());
        s.dispatch(Action::Mix(MixOp::Reject)).unwrap();
        assert_eq!(s.project().tracks, tracks);
    }
}

/// The resolution is configurable within limits; another resolution is
/// measured anew (its settings are part of the cache key).
#[test]
fn the_resolution_is_configurable_and_keys_the_cache() {
    use faderframe_mix_intelligence::AnalysisConfig;
    let (mut s, _, _) = {
        let (s, [a, b, _]) = session();
        (s, a, b)
    };
    let bad = AnalysisConfig {
        fft_size: 1000,
        ..AnalysisConfig::for_rate(SR)
    };
    assert!(s.set_mix_config(Some(bad)).is_err());
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let finer = AnalysisConfig {
        fft_size: 16_384,
        hop: 4096,
        ..AnalysisConfig::for_rate(SR)
    };
    s.set_mix_config(Some(finer)).unwrap();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!(r.config.fft_size, 16_384);
    assert_eq!((r.rendered, r.reused), (4, 0));
    assert!(!masking(&s).is_empty());
    // Back to the default: the first profiles are still there.
    s.set_mix_config(None).unwrap();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    assert_eq!(s.mix_results().unwrap().reused, 4);
}
