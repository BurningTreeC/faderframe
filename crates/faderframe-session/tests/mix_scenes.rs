//! Mix scenes in the session: a recall sets the mix and is one undo step;
//! a crossfade reaches its targets after its time, moving steadily, and is
//! one undo step (stopped by an edit, where it was); scope and safe tracks
//! are kept to; a morph is one undo step; a compare comes back to the mix
//! as it was, or keeps the side heard; next and previous; a song's scene;
//! devices' values; MIDI learn names.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{ChannelLayout, MixSceneId, ParameterId, PluginInstanceId, SendId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::scenes::{FadeLength, SceneFade, ScopePart, SwitchPoint};
use faderframe_project::{
    AuxSend, Command, MappingTarget, PluginRef, PluginSlot, Project, SceneControl, Track,
    TrackColor, TrackKind,
};
use faderframe_session::scenes::{SceneActivity, SceneOp, SceneSide};
use faderframe_session::{Action, AudioPreferences, Session};
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const SEND: SendId = SendId(900);
const COMP: PluginInstanceId = PluginInstanceId(901);

struct Ids {
    vox: TrackId,
    gtr: TrackId,
    aux: TrackId,
}

/// A vocal (with a compressor and a send to a reverb return) and a
/// guitar into the master.
fn session() -> (Session, Ids) {
    let mut p = Project::new("Scenes", SR);
    let vox: TrackId = p.ids.allocate();
    let gtr: TrackId = p.ids.allocate();
    let aux: TrackId = p.ids.allocate();
    let mut v = Track::new(vox, TrackKind::Audio, "Vox", TrackColor::palette(0))
        .with_layout(ChannelLayout::Stereo);
    v.sends.push(AuxSend {
        id: SEND,
        target: aux,
        level_db: -12.0,
        tap: Default::default(),
        enabled: true,
        pan: None,
    });
    v.inserts.push(PluginSlot {
        id: COMP,
        plugin: PluginRef::builtin(faderframe_core::builtin::COMPRESSOR, "Compressor"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    p.tracks.insert(0, v);
    p.tracks.insert(
        1,
        Track::new(gtr, TrackKind::Audio, "Gtr", TrackColor::palette(1)),
    );
    p.tracks.insert(
        2,
        Track::new(aux, TrackKind::Aux, "Verb", TrackColor::palette(2)),
    );
    p.ids.reserve_through(1000);
    let s = Session::new(p, None, EngineConfig::default()).unwrap();
    (s, Ids { vox, gtr, aux })
}

fn op(s: &mut Session, op: SceneOp) {
    s.dispatch(Action::MixScene(op)).unwrap();
}

fn edit(s: &mut Session, c: Command) {
    s.dispatch(Action::Edit(c)).unwrap();
}

fn store(s: &mut Session, name: &str) -> MixSceneId {
    op(
        s,
        SceneOp::Store {
            name: Some(name.into()),
            plugins: false,
        },
    );
    s.mix_scenes().scenes.last().unwrap().id
}

fn steps(s: &Session) -> usize {
    s.history_steps().0.len()
}

fn volume(s: &Session, t: TrackId) -> f32 {
    s.project().track(t).unwrap().volume_db
}

fn fade(ms: f64, switch: SwitchPoint) -> Option<SceneFade> {
    Some(SceneFade {
        length: FadeLength::Millis { ms },
        switch,
    })
}

/// Tick until `done` (real time), up to a few seconds.
fn wait(s: &mut Session, mut done: impl FnMut(&mut Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_recall_sets_the_mix_in_one_undo_step() {
    let (mut s, ids) = session();
    let verse = store(&mut s, "Verse");
    assert_eq!(s.mix_scenes().scenes.len(), 1);
    assert_eq!(s.current_mix_scene(), Some(verse));
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -10.0,
        },
    );
    edit(
        &mut s,
        Command::SetTrackMute {
            track: ids.gtr,
            on: true,
        },
    );
    edit(
        &mut s,
        Command::SetSendLevel {
            track: ids.vox,
            send: SEND,
            db: 0.0,
        },
    );
    edit(
        &mut s,
        Command::SetPluginBypass {
            track: ids.vox,
            plugin: COMP,
            bypass: true,
        },
    );
    assert!(!s.mix_scene_matches(verse));
    let moved = s.project().clone();
    let before = steps(&s);
    op(
        &mut s,
        SceneOp::Recall {
            scene: verse,
            fade: None,
        },
    );
    assert_eq!(steps(&s), before + 1, "one undo step");
    assert!(s.history_steps().0.last().unwrap().contains("Verse"));
    assert_eq!(volume(&s, ids.vox), 0.0);
    let p = s.project();
    assert!(!p.track(ids.gtr).unwrap().mute);
    assert_eq!(p.track(ids.vox).unwrap().sends[0].level_db, -12.0);
    assert!(!p.track(ids.vox).unwrap().inserts[0].bypass);
    assert!(s.mix_scene_matches(verse));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(*s.project(), moved);
}

#[test]
fn scope_and_safe_tracks_are_kept_to() {
    let (mut s, ids) = session();
    let a = store(&mut s, "A");
    op(
        &mut s,
        SceneOp::SetSafe {
            scene: a,
            track: ids.gtr,
            safe: true,
        },
    );
    for part in [ScopePart::Mutes, ScopePart::Sends] {
        op(
            &mut s,
            SceneOp::SetScope {
                scene: a,
                part,
                on: false,
            },
        );
    }
    let scene = s.mix_scenes().get(a).unwrap().clone();
    assert!(scene.safe.contains(&ids.gtr));
    assert!(!scene.scope.has(ScopePart::Mutes) && scene.scope.has(ScopePart::Faders));
    for (track, db) in [(ids.vox, -20.0), (ids.gtr, -20.0)] {
        edit(&mut s, Command::SetTrackVolume { track, db });
    }
    edit(
        &mut s,
        Command::SetTrackMute {
            track: ids.vox,
            on: true,
        },
    );
    edit(
        &mut s,
        Command::SetSendLevel {
            track: ids.vox,
            send: SEND,
            db: -3.0,
        },
    );
    op(
        &mut s,
        SceneOp::Recall {
            scene: a,
            fade: None,
        },
    );
    let p = s.project();
    assert_eq!(volume(&s, ids.vox), 0.0, "faders are in scope");
    assert_eq!(volume(&s, ids.gtr), -20.0, "the guitar is safe");
    assert!(p.track(ids.vox).unwrap().mute, "mutes are out of scope");
    assert_eq!(p.track(ids.vox).unwrap().sends[0].level_db, -3.0);
    // The menu offers the guitar's safety, checked.
    let menu = s.mix_scene_menu(ids.gtr);
    let safe = menu
        .iter()
        .find(|e| e.label.contains("Safe from ‘A’"))
        .unwrap();
    assert_eq!(safe.checked, Some(true));
}

/// Without a stream the ticks move the faders: down steadily, between
/// the ends, the targets exactly after the time, the switched values at
/// the switch point — and one undo step back to where it began.
#[test]
fn a_crossfade_reaches_its_targets_after_its_time() {
    let (mut s, ids) = session();
    let quiet = {
        edit(
            &mut s,
            Command::SetTrackVolume {
                track: ids.vox,
                db: -30.0,
            },
        );
        edit(
            &mut s,
            Command::SetTrackMute {
                track: ids.gtr,
                on: true,
            },
        );
        edit(
            &mut s,
            Command::SetTrackPan {
                track: ids.gtr,
                pan: -1.0,
            },
        );
        store(&mut s, "Quiet")
    };
    for c in [
        Command::SetTrackVolume {
            track: ids.vox,
            db: 0.0,
        },
        Command::SetTrackMute {
            track: ids.gtr,
            on: false,
        },
        Command::SetTrackPan {
            track: ids.gtr,
            pan: 1.0,
        },
    ] {
        edit(&mut s, c);
    }
    let loud = s.project().clone();
    let before = steps(&s);
    let started = Instant::now();
    op(
        &mut s,
        SceneOp::Recall {
            scene: quiet,
            fade: fade(400.0, SwitchPoint::Middle),
        },
    );
    let mut seen: Vec<(f64, f32, f32, bool)> = Vec::new();
    wait(
        &mut s,
        |s| {
            let t = started.elapsed().as_secs_f64() / 0.4;
            let p = s.project();
            let g = p.track(ids.gtr).unwrap();
            seen.push((t, volume(s, ids.vox), g.pan, g.mute));
            s.scene_activity().is_none()
        },
        "the crossfade",
    );
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert!(seen.len() > 10, "{}", seen.len());
    for w in seen.windows(2) {
        assert!(w[1].1 <= w[0].1 + 1e-4, "the fader falls: {w:?}");
        assert!(w[1].2 <= w[0].2 + 1e-6, "the pan moves left: {w:?}");
    }
    assert!(
        seen.iter().any(|(_, v, _, _)| *v < -1.0 && *v > -29.0),
        "on its way"
    );
    // Muted from halfway (ticks a little late, never early).
    for (t, _, _, mute) in &seen {
        if *mute {
            assert!(*t >= 0.5, "muted at {t}");
        }
        if *t < 0.45 {
            assert!(!*mute, "muted early at {t}");
        }
    }
    assert_eq!(volume(&s, ids.vox), -30.0, "exactly the scene");
    assert_eq!(s.project().track(ids.gtr).unwrap().pan, -1.0);
    assert!(s.project().track(ids.gtr).unwrap().mute);
    assert_eq!(steps(&s), before + 1, "one undo step");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(*s.project(), loud);
}

/// An edit during a crossfade stops it where it was (one step), then
/// applies; a crossfade in beats takes the tempo's time; the project's
/// default recall is the scene's unless it has its own.
#[test]
fn an_edit_stops_a_crossfade_where_it_is() {
    let (mut s, ids) = session();
    let a = store(&mut s, "A");
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -24.0,
        },
    );
    let before = steps(&s);
    // The project's recall: 8 beats at 120 BPM = 4 s.
    op(
        &mut s,
        SceneOp::SetFade {
            scene: None,
            fade: Some(SceneFade {
                length: FadeLength::Beats { beats: 8.0 },
                switch: SwitchPoint::End,
            }),
        },
    );
    op(
        &mut s,
        SceneOp::Recall {
            scene: a,
            fade: None,
        },
    );
    std::thread::sleep(Duration::from_millis(300));
    s.tick(0.005);
    let Some(SceneActivity::Fading { progress, .. }) = s.scene_activity() else {
        panic!("fading");
    };
    assert!(progress > 0.05 && progress < 0.3, "{progress}");
    let midway = volume(&s, ids.vox);
    assert!(midway > -24.0 && midway < 0.0, "{midway}");
    edit(
        &mut s,
        Command::SetTrackPan {
            track: ids.gtr,
            pan: 0.5,
        },
    );
    assert!(s.scene_activity().is_none());
    let labels = s.history_steps().0;
    assert_eq!(
        labels.len(),
        before + 3,
        "options, the stopped fade, the pan"
    );
    assert!(labels[labels.len() - 2].contains("Stopped"), "{labels:?}");
    let stopped = volume(&s, ids.vox);
    assert!((stopped - midway).abs() < 3.0, "{stopped} vs {midway}");
    // Undo the pan, then the stopped recall: back where it began.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(volume(&s, ids.vox), -24.0);
}

/// The sound glides on the audio thread while a stream runs; the
/// session's faders follow, and the crossfade ends on the scene in one
/// step.
#[test]
fn a_crossfade_with_a_stream_running_glides_and_ends_on_the_scene() {
    let (mut s, ids) = session();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    let a = store(&mut s, "A");
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -40.0,
        },
    );
    edit(
        &mut s,
        Command::SetSendLevel {
            track: ids.vox,
            send: SEND,
            db: -40.0,
        },
    );
    let before = steps(&s);
    op(
        &mut s,
        SceneOp::Recall {
            scene: a,
            fade: fade(300.0, SwitchPoint::Start),
        },
    );
    let mut last = -41.0f32;
    wait(
        &mut s,
        |s| {
            let v = volume(s, ids.vox);
            assert!(v >= last - 1e-4, "the fader rises: {last} then {v}");
            last = v;
            s.scene_activity().is_none()
        },
        "the crossfade",
    );
    assert_eq!(volume(&s, ids.vox), 0.0);
    assert_eq!(s.project().track(ids.vox).unwrap().sends[0].level_db, -12.0);
    assert_eq!(steps(&s), before + 1);
    // Ticks after the end let the glide go and keep the values.
    for _ in 0..20 {
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(volume(&s, ids.vox), 0.0);
    s.stop_audio();
}

#[test]
fn a_morph_is_one_undo_step() {
    let (mut s, ids) = session();
    let a = store(&mut s, "A");
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -20.0,
        },
    );
    edit(
        &mut s,
        Command::SetTrackPan {
            track: ids.gtr,
            pan: 1.0,
        },
    );
    let b = store(&mut s, "B");
    op(
        &mut s,
        SceneOp::SetAB {
            a: Some(a),
            b: Some(b),
        },
    );
    let start = s.project().clone();
    let before = steps(&s);
    for pos in [0.1, 0.3, 0.5] {
        op(&mut s, SceneOp::Morph { position: pos });
    }
    assert!(matches!(
        s.scene_activity(),
        Some(SceneActivity::Morphing { position, .. }) if position == 0.5
    ));
    let v = volume(&s, ids.vox);
    assert!(v < 0.0 && v > -20.0, "{v}");
    assert!((s.project().track(ids.gtr).unwrap().pan - 0.5).abs() < 1e-5);
    assert_eq!(steps(&s), before, "nothing recorded while morphing");
    op(&mut s, SceneOp::EndMorph);
    assert_eq!(steps(&s), before + 1, "one undo step");
    assert!(s.history_steps().0.last().unwrap().contains("Morph"));
    assert!((s.project().track(ids.gtr).unwrap().pan - 0.5).abs() < 1e-5);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(*s.project(), start);
    // Without A and B there is nothing to morph between.
    op(&mut s, SceneOp::SetAB { a: None, b: None });
    assert!(
        s.dispatch(Action::MixScene(SceneOp::Morph { position: 0.5 }))
            .is_err()
    );
}

/// Comparing the mix with a scene: the scene heard, the mix back as it
/// was (nothing recorded, unsaved changes kept), or the scene kept as one
/// step; an edit while comparing keeps the side heard.
#[test]
fn a_compare_keeps_the_mix_as_it_was() {
    let (mut s, ids) = session();
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -18.0,
        },
    );
    let b = store(&mut s, "B");
    // Unsaved mix changes since.
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -3.0,
        },
    );
    edit(
        &mut s,
        Command::SetTrackSolo {
            track: ids.gtr,
            on: true,
        },
    );
    let mix = s.project().clone();
    let before = steps(&s);
    op(&mut s, SceneOp::CompareStart { a: None, b });
    assert_eq!(volume(&s, ids.vox), -18.0, "B is heard");
    assert!(!s.project().track(ids.gtr).unwrap().solo);
    op(&mut s, SceneOp::CompareFlip);
    assert_eq!(*s.project(), mix, "the mix as it was");
    op(&mut s, SceneOp::CompareHear(SceneSide::B));
    assert_eq!(volume(&s, ids.vox), -18.0);
    op(&mut s, SceneOp::CompareEnd { keep: false });
    assert_eq!(*s.project(), mix);
    assert_eq!(steps(&s), before, "nothing recorded");
    // Kept: one step.
    op(&mut s, SceneOp::CompareStart { a: None, b });
    op(&mut s, SceneOp::CompareEnd { keep: true });
    assert_eq!(volume(&s, ids.vox), -18.0);
    assert_eq!(steps(&s), before + 1);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(*s.project(), mix);
    // An edit while hearing B keeps B, then applies.
    op(&mut s, SceneOp::CompareStart { a: None, b });
    edit(
        &mut s,
        Command::SetTrackPan {
            track: ids.aux,
            pan: 0.3,
        },
    );
    assert!(s.scene_activity().is_none());
    assert_eq!(volume(&s, ids.vox), -18.0);
    assert_eq!(steps(&s), before + 2);
}

#[test]
fn next_and_previous_go_along_the_list() {
    let (mut s, ids) = session();
    let mut scenes = Vec::new();
    for (i, db) in [0.0, -6.0, -12.0].into_iter().enumerate() {
        edit(&mut s, Command::SetTrackVolume { track: ids.vox, db });
        scenes.push(store(&mut s, &format!("S{i}")));
    }
    op(
        &mut s,
        SceneOp::Recall {
            scene: scenes[0],
            fade: None,
        },
    );
    op(&mut s, SceneOp::Next);
    assert_eq!(s.current_mix_scene(), Some(scenes[1]));
    assert_eq!(volume(&s, ids.vox), -6.0);
    op(&mut s, SceneOp::Next);
    op(&mut s, SceneOp::Next);
    assert_eq!(s.current_mix_scene(), Some(scenes[2]), "stays on the last");
    op(&mut s, SceneOp::Previous);
    assert_eq!(volume(&s, ids.vox), -6.0);
    // Rename, move, delete.
    op(&mut s, SceneOp::Rename(scenes[2], "Outro".into()));
    op(
        &mut s,
        SceneOp::Move {
            scene: scenes[2],
            to: 0,
        },
    );
    let names: Vec<_> = s
        .mix_scenes()
        .scenes
        .iter()
        .map(|x| x.name.clone())
        .collect();
    assert_eq!(names, ["Outro", "S0", "S1"]);
    op(&mut s, SceneOp::Delete(scenes[1]));
    assert_eq!(s.mix_scenes().scenes.len(), 2);
    assert_eq!(s.current_mix_scene(), None);
    // Controller targets name the scene.
    assert_eq!(
        s.mapping_target_label(&MappingTarget::MixScene {
            control: SceneControl::Recall { scene: scenes[0] }
        }),
        "Mix Scenes · Recall ‘S0’"
    );
    assert_eq!(
        s.mapping_target_label(&MappingTarget::MixScene {
            control: SceneControl::Morph
        }),
        "Mix Scenes · Morph A–B"
    );
}

/// A scene kept with the devices' values sets them back; one without
/// leaves them alone.
#[test]
fn a_scene_keeps_the_devices_values_when_asked() {
    let (mut s, ids) = session();
    let threshold = ParameterId(0);
    let set = |s: &mut Session, v: f64| {
        s.dispatch(Action::Edit(Command::SetPluginParameter {
            track: ids.vox,
            plugin: COMP,
            parameter: threshold,
            value: Some(v),
        }))
        .unwrap();
    };
    set(&mut s, -30.0);
    op(
        &mut s,
        SceneOp::Store {
            name: Some("With".into()),
            plugins: true,
        },
    );
    let with = s.mix_scenes().scenes[0].clone();
    assert!(with.mix.keeps_plugins() && with.scope.has(ScopePart::Plugins));
    let without = store(&mut s, "Without");
    set(&mut s, -10.0);
    op(
        &mut s,
        SceneOp::Recall {
            scene: without,
            fade: None,
        },
    );
    let value = |s: &Session| {
        s.project().track(ids.vox).unwrap().inserts[0]
            .parameters
            .iter()
            .find(|p| p.id == threshold)
            .unwrap()
            .value
    };
    assert_eq!(value(&s), -10.0);
    op(
        &mut s,
        SceneOp::Recall {
            scene: with.id,
            fade: None,
        },
    );
    assert_eq!(value(&s), -30.0);
}

/// Show mode standing on a song recalls the scene it names.
#[test]
fn a_song_recalls_its_scene() {
    use faderframe_session::setlist::{SetlistOp, ShowOp};
    use faderframe_timeline::MusicalTime;
    let (mut s, ids) = session();
    let one = store(&mut s, "One");
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: ids.vox,
            db: -9.0,
        },
    );
    let two = store(&mut s, "Two");
    for (name, a, b) in [("A", 0.0, 4.0), ("B", 4.0, 8.0)] {
        s.dispatch(Action::Setlist(SetlistOp::Add {
            name: name.into(),
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        }))
        .unwrap();
    }
    s.dispatch(Action::Setlist(SetlistOp::Scene(0, Some(one))))
        .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::Scene(1, Some(two))))
        .unwrap();
    s.dispatch(Action::Show(ShowOp::Enter)).unwrap();
    assert_eq!(volume(&s, ids.vox), 0.0, "song A's scene");
    s.dispatch(Action::Show(ShowOp::Next)).unwrap();
    assert_eq!(volume(&s, ids.vox), -9.0, "song B's scene");
    assert_eq!(s.current_mix_scene(), Some(two));
    // A deleted scene leaves the song recalling nothing.
    op(&mut s, SceneOp::Delete(two));
    assert_eq!(s.setlist().songs[1].scene, None);
}
