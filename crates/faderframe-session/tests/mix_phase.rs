//! Mix Intelligence's phase, timing, width and dynamics in a session: pairs
//! rendered over excerpts (processed, or before processing), polarity and
//! delay found and corrected by the track's own delay and polarity —
//! auditioned unrecorded, rejected, applied as one undo step, undone —,
//! the pair measurements cached, transient flattening against the source.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{ChannelLayout, ParameterId, TrackId, TrackRole, builtin};
use faderframe_engine::EngineConfig;
use faderframe_mix_intelligence::{Experiment, Finding, RuleId, SignalPoint, Subject};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, PluginRef, PluginSlot, Project,
    SavedParameter, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::mix::{MixOp, MixScope};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

/// A track playing `spec` from the start (generated sources play at a
/// peak of 0.7).
fn generated(p: &mut Project, name: &str, spec: GeneratorSpec, role: Option<TrackRole>) -> TrackId {
    let frames = (spec.seconds() * f64::from(SR)) as i64;
    let stereo = spec.channels() == 2;
    let src = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: name.into(),
            spec: SourceSpec::Generated { generator: spec },
        },
    );
    let id: TrackId = p.ids.allocate();
    let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(1));
    if stereo {
        t = t.with_layout(ChannelLayout::Stereo);
    }
    t.role = role;
    let clip = Clip {
        id: p.ids.allocate(),
        track: id,
        name: name.into(),
        color: None,
        start: MusicalTime::ZERO,
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source: src,
            source_offset: 0,
            length: frames,
            gain_db: -6.0,
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

fn drums(seed: u64) -> GeneratorSpec {
    GeneratorSpec::DrumLoop {
        bpm: 120.0,
        bars: 4,
        seed,
    }
}

fn track_mut(p: &mut Project, id: TrackId) -> &mut Track {
    p.tracks.iter_mut().find(|t| t.id == id).unwrap()
}

/// Two microphones on one snare (the same drum loop), as `edit` makes them.
fn snare_pair(edit: impl Fn(&mut Track)) -> (Session, TrackId, TrackId) {
    let mut p = Project::new("Phase", SR);
    let a = generated(&mut p, "Snare Top", drums(7), Some(TrackRole::Snare));
    let b = generated(&mut p, "Snare Bottom", drums(7), Some(TrackRole::Snare));
    edit(track_mut(&mut p, b));
    (
        Session::new(p, None, EngineConfig::default()).unwrap(),
        a,
        b,
    )
}

fn analyse(s: &mut Session, signal: SignalPoint) {
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal,
    }))
    .unwrap();
    s.wait_for_mix();
    assert!(s.mix_results().is_some(), "{:?}", s.mix_status());
}

fn of_rule(s: &Session, rule: RuleId) -> Vec<Finding> {
    s.mix_results()
        .unwrap()
        .report
        .findings
        .iter()
        .filter(|f| f.rule == rule)
        .cloned()
        .collect()
}

fn alignment(f: &Finding) -> (usize, TrackId, f64, bool) {
    f.recommendations
        .iter()
        .enumerate()
        .find_map(|(i, r)| match r.experiment {
            Experiment::Alignment {
                track,
                change_ms,
                invert,
            } => Some((i, track, change_ms, invert)),
            _ => None,
        })
        .unwrap()
}

#[test]
fn an_inverted_microphone_is_found_auditioned_rejected_applied_and_undone() {
    // The other way up and 3 dB quieter (fully equal and inverted, the two
    // would leave the master silent: no loudness to match).
    let (mut s, a, b) = snare_pair(|t| {
        t.phase_invert = true;
        t.volume_db = -3.0;
    });
    analyse(&mut s, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!(
        r.report.details.pairs.len(),
        1,
        "the two snares are compared"
    );
    assert_eq!(r.extra.pairs_measured, 1);
    let found = of_rule(&s, RuleId::PolarityInverted);
    assert_eq!(found.len(), 1, "{:#?}", r.report.findings);
    let f = found[0].clone();
    assert_eq!(f.subject, Subject::Tracks { a, b });
    let (i, track, change, invert) = alignment(&f);
    assert!(invert && change.abs() < 0.01, "{change}");
    assert!(track == a || track == b);
    let tracks = s.project().tracks.clone();
    let steps = s.history_steps().0.len();
    // Audition: the polarity turns over in the engine, no undo step.
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps);
    let flipped = |s: &Session| s.project().track(track).unwrap().phase_invert;
    let was = tracks.iter().find(|t| t.id == track).unwrap().phase_invert;
    assert_eq!(flipped(&s), !was);
    // A/B: the original as it was, then the candidate again.
    s.dispatch(Action::Mix(MixOp::Compare(false))).unwrap();
    assert_eq!(flipped(&s), was);
    s.dispatch(Action::Mix(MixOp::Compare(true))).unwrap();
    assert_eq!(flipped(&s), !was);
    // The loudness is measured with and without it: the corrected pair
    // is louder (they no longer cancel).
    s.wait_for_mix_measure();
    let m = s.mix_audition().unwrap().measured.unwrap().unwrap();
    assert!(m.change() > 3.0, "{m:?}");
    // Rejected: the project as it was.
    s.dispatch(Action::Mix(MixOp::Reject)).unwrap();
    assert_eq!(s.project().tracks, tracks);
    // Applied: one step; the two now agree.
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    let (done, _) = s.history_steps();
    assert_eq!(done.len(), steps + 1);
    assert!(
        done.last().unwrap().starts_with("Mix Intelligence:"),
        "{done:?}"
    );
    let pa = s.project().track(a).unwrap().phase_invert;
    let pb = s.project().track(b).unwrap().phase_invert;
    assert_eq!(pa, pb);
    // Stale until analysed again; then nothing to correct.
    s.tick(0.016);
    assert!(s.mix_stale().is_some());
    assert!(s.mix_can_try(f.id, i).is_err());
    analyse(&mut s, SignalPoint::Processed);
    assert!(of_rule(&s, RuleId::PolarityInverted).is_empty());
    // Undo: as it was.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks, tracks);
}

#[test]
fn a_track_delay_is_found_to_a_fraction_of_a_sample_and_taken_out() {
    // The bottom microphone heard 0.5 ms (24 samples) late through its
    // track delay.
    let (mut s, _a, b) = snare_pair(|t| t.delay_ms = 0.5);
    analyse(&mut s, SignalPoint::Processed);
    let found = of_rule(&s, RuleId::TimeOffset);
    assert_eq!(
        found.len(),
        1,
        "{:#?}",
        s.mix_results().unwrap().report.findings
    );
    let f = found[0].clone();
    let (i, track, change, invert) = alignment(&f);
    assert_eq!(track, b, "the later one moves earlier");
    assert!((change + 0.5).abs() < 0.005 && !invert, "{change}");
    let steps = s.history_steps().0.len();
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let d = s.project().track(b).unwrap().delay_ms;
    assert!(d.abs() < 0.005, "{d}");
    analyse(&mut s, SignalPoint::Processed);
    assert!(
        of_rule(&s, RuleId::TimeOffset).is_empty(),
        "{:#?}",
        s.mix_results().unwrap().report.findings
    );
    s.dispatch(Action::Undo).unwrap();
    assert!((s.project().track(b).unwrap().delay_ms - 0.5).abs() < 1e-9);
}

#[test]
fn before_processing_the_track_s_polarity_is_not_part_of_it_and_pairs_are_cached() {
    let (mut s, a, _b) = snare_pair(|t| t.phase_invert = true);
    // Sources leave the tracks' polarity out: the same signal, nothing to
    // correct.
    analyse(&mut s, SignalPoint::Source);
    assert!(of_rule(&s, RuleId::PolarityInverted).is_empty());
    let f = s
        .mix_results()
        .unwrap()
        .report
        .findings
        .iter()
        .filter(|f| f.category() == faderframe_mix_intelligence::Category::Phase)
        .count();
    assert_eq!(f, 0);
    assert_eq!(s.mix_results().unwrap().extra.pairs_measured, 1);
    // Analysed again unchanged: the pair is reused, not rendered again.
    analyse(&mut s, SignalPoint::Source);
    let r = s.mix_results().unwrap();
    assert_eq!((r.extra.pairs_measured, r.extra.pairs_reused), (0, 1));
    // A rename changes nothing heard: still reused.
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: a,
        name: "Top".into(),
    }))
    .unwrap();
    analyse(&mut s, SignalPoint::Source);
    assert_eq!(s.mix_results().unwrap().extra.pairs_reused, 1);
    // Processed is another signal: measured anew.
    analyse(&mut s, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!((r.extra.pairs_measured, r.extra.pairs_reused), (1, 0));
    // Every track and the master have their width and dynamics.
    assert!(r.report.details.width.len() >= 3);
    assert!(
        r.report
            .details
            .dynamics
            .iter()
            .any(|d| matches!(d.subject, Subject::Master(_)))
    );
}

fn compressor(p: &mut Project, track: TrackId, params: &[(u32, f64)]) {
    let slot = PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(builtin::COMPRESSOR, "Compressor"),
        bypass: false,
        parameters: params
            .iter()
            .map(|&(id, value)| SavedParameter {
                id: ParameterId(id),
                value,
            })
            .collect(),
        state: None,
        sidechain: None,
    };
    track_mut(p, track).inserts.push(slot);
}

#[test]
fn a_hard_compressor_possibly_flattens_the_attacks_and_a_gentle_one_does_not() {
    use faderframe_plugin_host::devices::compressor::id;
    let mut p = Project::new("Dynamics", SR);
    let hard = generated(&mut p, "Hard", drums(3), Some(TrackRole::Drums));
    let gentle = generated(&mut p, "Gentle", drums(4), Some(TrackRole::Percussion));
    // Vintage (on the peaks, very fast), far over the threshold.
    compressor(
        &mut p,
        hard,
        &[
            (id::THRESHOLD, -45.0),
            (id::RATIO, 20.0),
            (id::ATTACK, 0.05),
            (id::RELEASE, 40.0),
            (id::STYLE, 3.0),
            (id::MAKEUP, 20.0),
        ],
    );
    // Barely touching it.
    compressor(
        &mut p,
        gentle,
        &[(id::THRESHOLD, 0.0), (id::RATIO, 1.5), (id::ATTACK, 30.0)],
    );
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    analyse(&mut s, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!(r.extra.sources_rendered, 2, "{:?}", r.extra);
    let flat = of_rule(&s, RuleId::TransientFlattening);
    assert_eq!(flat.len(), 1, "{:#?}", r.report.details.dynamics);
    let f = &flat[0];
    assert_eq!(f.subject, Subject::Track(hard));
    assert!(f.title(&|t| s.mix_track_name(t)).contains("possible"));
    assert!(
        f.recommendations
            .iter()
            .all(|x| !x.experiment.previewable())
    );
    let d = r
        .report
        .details
        .dynamics_of(Subject::Track(gentle))
        .and_then(|d| d.flattening)
        .unwrap();
    assert!(d.tsr_change_db > -2.0, "{d:?}");
    // The sources are cached with a source analysis' keys: analysing the
    // sources reuses them.
    analyse(&mut s, SignalPoint::Source);
    let r = s.mix_results().unwrap();
    assert_eq!(r.reused, 2, "{} rendered", r.rendered);
    // And a processed analysis again reuses everything.
    analyse(&mut s, SignalPoint::Processed);
    let r = s.mix_results().unwrap();
    assert_eq!((r.extra.sources_rendered, r.extra.sources_reused), (0, 2));
}

/// A copy of a track, panned and quieter, carries the same audio: no
/// masking between the two, a finding saying so, and the pair compared for
/// phase and timing.
#[test]
fn a_panned_copy_is_the_same_audio_not_masking() {
    let mut p = Project::new("Copy", SR);
    let a = generated(&mut p, "Loop", drums(5), None);
    let b = generated(&mut p, "Loop Copy", drums(5), None);
    {
        let t = track_mut(&mut p, b);
        t.pan = -0.8;
        t.volume_db = -4.0;
    }
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    for signal in [SignalPoint::Processed, SignalPoint::Source] {
        analyse(&mut s, signal);
        let r = s.mix_results().unwrap();
        assert!(
            of_rule(&s, RuleId::SpectralMasking).is_empty(),
            "{signal:?}: {:#?}",
            of_rule(&s, RuleId::SpectralMasking)
        );
        let same = of_rule(&s, RuleId::SameAudio);
        assert_eq!(same.len(), 1, "{signal:?}: {:#?}", r.report.findings);
        assert_eq!(same[0].subject, Subject::Tracks { a, b });
        let title = same[0].title(&|t| s.mix_track_name(t));
        assert!(
            title.contains("Loop Copy") && title.contains("same audio"),
            "{title}"
        );
        assert!(
            r.report.details.pairs.iter().any(|x| x
                .reasons
                .contains(&faderframe_mix_intelligence::PairReason::SameAudio)),
            "compared for phase and timing too"
        );
    }
}
