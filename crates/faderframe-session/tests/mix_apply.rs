//! Mix Intelligence after an experiment is applied: the tracks it changed
//! analysed again at once (alone first, the master after; their pairs from
//! the signals held from the last render), the applied state ("Applied",
//! "Resolved") following the undo history, and the Undo that takes an
//! experiment back — the last step undone, an older one reverted alone.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_mix_intelligence::{Experiment, Finding, RuleId, SignalPoint};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Project, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::mix::{Applied, MixOp, MixScope};
use faderframe_session::{Action, Session};

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

fn eq_count(s: &Session, t: TrackId) -> usize {
    s.project()
        .track(t)
        .unwrap()
        .inserts
        .iter()
        .filter(|x| x.plugin.id == faderframe_core::builtin::EQ)
        .count()
}

/// The masking finding and its EQ cut: (finding, recommendation, masker).
fn eq_cut(s: &Session) -> (Finding, usize, TrackId) {
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
    (f, i, masker)
}

#[test]
fn applying_analyses_the_changed_tracks_again_and_undo_takes_it_back() {
    let (mut s, _) = session();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    // The one pass held the signals: the pairs read their excerpts there.
    let r = s.mix_results().unwrap();
    assert!(r.extra.pairs_measured > 0, "{:?}", r.extra);
    assert_eq!(r.extra.excerpt_renders, 0, "{:?}", r.extra);
    assert!(r.extra.excerpts_held >= 2, "{:?}", r.extra);
    let (f, i, masker) = eq_cut(&s);
    assert_eq!(s.mix_applied_state(f.id), None);
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert_eq!(eq_count(&s, masker), 1);
    // Analysed again at once, without being asked.
    assert!(s.mix_analysing());
    let applied = s.mix_applied_to(f.id);
    assert_eq!(applied.len(), 1);
    assert!(applied[0].label.starts_with("Mix Intelligence: EQ cut on"));
    assert_eq!(s.mix_applied_state(f.id), Some(Applied::Checking));
    s.wait_for_mix();
    let r = s.mix_results().unwrap();
    assert!(r.complete);
    // The masker alone first, then the whole mix for the master; the
    // others reused; the pairs from held signals (none rendered).
    assert!(
        r.phases
            .iter()
            .any(|(n, _)| n.contains("the changed tracks")),
        "{:?}",
        r.phases
    );
    assert_eq!((r.rendered, r.reused, r.renders), (2, 2, 2));
    assert_eq!(r.extra.excerpt_renders, 0, "{:?}", r.extra);
    assert!(matches!(
        s.mix_applied_state(f.id),
        Some(Applied::Found | Applied::Resolved)
    ));
    // The applied experiment cannot be applied twice.
    if s.mix_finding(f.id).is_some() && s.mix_applied(f.id, i) {
        assert!(s.mix_can_try(f.id, i).is_err());
    }
    // Its Undo: the step undone (it was the last), the badge gone, and the
    // project as it was analysed — everything reused.
    let step = applied[0].step;
    s.dispatch(Action::Mix(MixOp::Undo(step))).unwrap();
    assert_eq!(eq_count(&s, masker), 0);
    assert!(s.mix_applied_to(f.id).is_empty());
    assert_eq!(s.mix_applied_state(f.id), None);
    let (_, redo) = s.history_steps();
    assert!(
        redo[0].starts_with("Mix Intelligence: EQ cut on"),
        "{redo:?}"
    );
    assert!(s.mix_analysing());
    s.wait_for_mix();
    let r = s.mix_results().unwrap();
    assert_eq!((r.rendered, r.renders), (0, 0), "as it was: all reused");
    assert!(s.mix_finding(f.id).is_some(), "the finding is back");
    // Redo brings the experiment (and its badge) back, analysed again.
    s.dispatch(Action::Redo).unwrap();
    s.tick(0.016);
    assert_eq!(eq_count(&s, masker), 1);
    assert_eq!(s.mix_applied_to(f.id).len(), 1);
    assert!(s.mix_analysing(), "a redo of it analyses again");
    s.wait_for_mix();
    // Undo from the menu (Ctrl+Z) takes the badge too.
    s.dispatch(Action::Undo).unwrap();
    s.tick(0.016);
    assert!(s.mix_applied_to(f.id).is_empty());
    s.wait_for_mix();
}

#[test]
fn an_experiment_applied_before_other_edits_is_taken_back_alone() {
    let (mut s, [_, _, h]) = session();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let (f, i, masker) = eq_cut(&s);
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    s.wait_for_mix();
    let step = s.mix_applied_to(f.id)[0].step;
    // Something else after it.
    s.dispatch(Action::Edit(Command::SetTrackVolume { track: h, db: -4.0 }))
        .unwrap();
    s.tick(0.016);
    s.wait_for_mix();
    s.dispatch(Action::Mix(MixOp::Undo(step))).unwrap();
    assert_eq!(eq_count(&s, masker), 0, "the EQ is out");
    assert_eq!(
        s.project().track(h).unwrap().volume_db,
        -4.0,
        "the later edit stays"
    );
    let (done, _) = s.history_steps();
    assert!(
        done.last()
            .unwrap()
            .starts_with("Undo Mix Intelligence: EQ cut on"),
        "{done:?}"
    );
    assert!(s.mix_applied_to(f.id).is_empty());
    s.wait_for_mix();
    // Undoing the Undo brings the experiment back, applied again.
    s.dispatch(Action::Undo).unwrap();
    s.tick(0.016);
    assert_eq!(eq_count(&s, masker), 1);
    assert_eq!(s.mix_applied_to(f.id).len(), 1);
    s.wait_for_mix();
    // An Undo of a step no longer in the project is refused.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    s.tick(0.016);
    assert!(s.dispatch(Action::Mix(MixOp::Undo(step))).is_err());
    s.wait_for_mix();
}
