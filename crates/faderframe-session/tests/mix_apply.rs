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
            envelope: None,
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

/// The EQ an experiment puts in says so (`PluginSlot::mix_origin`: the
/// finding and the experiment), in the file too; Keep as My Own takes the
/// mark off — one undo step, the device staying.
#[test]
fn an_applied_slot_says_where_it_came_from_until_kept_as_ones_own() {
    let (mut s, _) = session();
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let (f, i, masker) = eq_cut(&s);
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    s.wait_for_mix();
    let slot = |s: &Session| {
        s.project()
            .track(masker)
            .unwrap()
            .inserts
            .iter()
            .find(|x| x.plugin.id == faderframe_core::builtin::EQ)
            .cloned()
            .unwrap()
    };
    let eq = slot(&s);
    let origin = eq.mix_origin.clone().expect("marked");
    assert!(origin.contains("EQ cut on"), "{origin}");
    // Kept in the file; an unmarked slot writes nothing new.
    let json = serde_json::to_string(&eq).unwrap();
    let back: faderframe_project::PluginSlot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.mix_origin.as_deref(), Some(origin.as_str()));
    let plain = faderframe_project::PluginSlot {
        mix_origin: None,
        ..eq.clone()
    };
    assert!(
        !serde_json::to_string(&plain)
            .unwrap()
            .contains("mix_origin")
    );
    // Keep as My Own: the mark goes, the EQ stays; undo brings it back.
    s.dispatch(Action::Edit(Command::SetPluginMixOrigin {
        track: masker,
        plugin: eq.id,
        origin: None,
    }))
    .unwrap();
    assert_eq!(slot(&s).mix_origin, None);
    assert_eq!(eq_count(&s, masker), 1);
    let (done, _) = s.history_steps();
    assert_eq!(done.last().map(String::as_str), Some("Keep as My Own"));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(slot(&s).mix_origin.as_deref(), Some(origin.as_str()));
    s.wait_for_mix();
}

/// An EQ experiment on a track whose devices end with a FaderFrame EQ can
/// go into that EQ's next free band instead of a new EQ (Apply Into): one
/// EQ still, the band as the experiment sets it, and its Undo frees the
/// band again. Not offered when something follows the EQ or it is off.
#[test]
fn an_eq_cut_goes_into_the_eq_the_track_ends_with() {
    use faderframe_plugin_host::eq::{Field, band_id};
    let (mut s, tracks) = session();
    // Each track ends with an EQ of its own whose first band is in use
    // (shown, not heard: nothing analysed changes).
    for t in tracks {
        s.dispatch(Action::InsertPlugin {
            track: t,
            index: 0,
            plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::EQ, "EQ"),
        })
        .unwrap();
        let eq = s.project().track(t).unwrap().inserts[0].id;
        s.dispatch(Action::Edit(Command::SetPluginParameter {
            track: t,
            plugin: eq,
            parameter: band_id(0, Field::Enabled),
            value: Some(2.0),
        }))
        .unwrap();
    }
    analyse(&mut s, MixScope::Project, SignalPoint::Processed);
    let (f, i, masker) = eq_cut(&s);
    let target = s.mix_merge_target(f.id, i).expect("offered");
    let eq = s.project().track(masker).unwrap().inserts[0].id;
    assert_eq!((target.plugin, target.band), (eq, 1));
    let Experiment::EqCut {
        freq_hz, gain_db, ..
    } = f.recommendations[i].experiment
    else {
        panic!("an EQ cut")
    };
    s.dispatch(Action::Mix(MixOp::ApplyInto {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    let saved = |s: &Session, field| {
        let t = s.project().track(masker).unwrap();
        t.inserts[0].saved(band_id(1, field))
    };
    assert_eq!(eq_count(&s, masker), 1, "no second EQ");
    assert_eq!(saved(&s, Field::Enabled), Some(1.0));
    assert_eq!(saved(&s, Field::Freq), Some(freq_hz));
    assert_eq!(saved(&s, Field::Gain), Some(gain_db));
    assert_eq!(
        s.project().track(masker).unwrap().inserts[0].mix_origin,
        None,
        "the user's EQ"
    );
    s.wait_for_mix();
    // Taken back: the band is free again.
    let step = s.mix_applied_to(f.id)[0].step;
    s.dispatch(Action::Mix(MixOp::Undo(step))).unwrap();
    assert_eq!(saved(&s, Field::Enabled), None);
    s.wait_for_mix();
    // Auditioned first (its own EQ after the user's), then put into the
    // user's EQ: the candidate's EQ goes, the band is in the user's.
    let (f, i, masker) = eq_cut(&s);
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert_eq!(eq_count(&s, masker), 2, "the candidate's own EQ");
    assert_eq!(s.mix_merge_target(f.id, i).map(|t| t.plugin), Some(eq));
    s.dispatch(Action::Mix(MixOp::ApplyInto {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    assert_eq!(eq_count(&s, masker), 1);
    assert_eq!(saved(&s, Field::Enabled), Some(1.0));
    s.wait_for_mix();
    let step = s.mix_applied_to(f.id)[0].step;
    s.dispatch(Action::Mix(MixOp::Undo(step))).unwrap();
    s.wait_for_mix();
    // Not offered with a device after the EQ, nor with the EQ off.
    let (f, i, masker) = eq_cut(&s);
    assert!(s.mix_merge_target(f.id, i).is_some());
    s.dispatch(Action::Edit(Command::SetPluginBypass {
        track: masker,
        plugin: eq,
        bypass: true,
    }))
    .unwrap();
    assert_eq!(s.mix_merge_target(f.id, i), None, "bypassed");
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::InsertPlugin {
        track: masker,
        index: 1,
        plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::GAIN, "Utility"),
    })
    .unwrap();
    assert_eq!(s.mix_merge_target(f.id, i), None, "a device after it");
    s.wait_for_mix();
}
