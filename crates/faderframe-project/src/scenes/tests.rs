use super::*;
use crate::{AuxSend, Impact, PluginRef, TrackColor, TrackKind};
use faderframe_core::ChannelLayout;

/// Two audio tracks into the master, one sending to an aux, an EQ on the
/// first.
fn project() -> (Project, TrackId, TrackId, TrackId) {
    let mut p = Project::new("Scenes", 48_000);
    let a: TrackId = p.ids.allocate();
    let b: TrackId = p.ids.allocate();
    let aux: TrackId = p.ids.allocate();
    let mut ta = Track::new(a, TrackKind::Audio, "Vox", TrackColor::palette(0))
        .with_layout(ChannelLayout::Stereo);
    ta.sends.push(AuxSend {
        id: SendId(900),
        target: aux,
        level_db: -12.0,
        tap: Default::default(),
        enabled: true,
        pan: None,
    });
    ta.inserts.push(PluginSlot {
        id: PluginInstanceId(901),
        plugin: PluginRef::builtin(faderframe_core::builtin::COMPRESSOR, "Compressor"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(0),
            value: -20.0,
        }],
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    p.tracks.insert(0, ta);
    p.tracks.insert(
        1,
        Track::new(b, TrackKind::Audio, "Gtr", TrackColor::palette(1)),
    );
    p.tracks.insert(
        2,
        Track::new(aux, TrackKind::Aux, "Verb", TrackColor::palette(2)),
    );
    p.ids.reserve_through(1000);
    (p, a, b, aux)
}

fn no_values(_: &Track, _: &PluginSlot) -> Option<DeviceValues> {
    None
}

fn with_values(_: &Track, s: &PluginSlot) -> Option<DeviceValues> {
    Some(DeviceValues {
        parameters: s.parameters.clone(),
        state: s.state.clone(),
    })
}

fn scene(p: &mut Project, name: &str, values: bool) -> MixScene {
    let mix = if values {
        SceneMix::capture(p, &mut with_values)
    } else {
        SceneMix::capture(p, &mut no_values)
    };
    MixScene {
        id: p.ids.allocate(),
        name: name.into(),
        mix,
        scope: SceneScope::default(),
        safe: BTreeSet::new(),
        fade: None,
    }
}

fn apply_all(p: &mut Project, commands: Vec<Command>) -> Command {
    Command::Batch {
        label: "Recall".into(),
        commands,
    }
    .apply(p)
    .unwrap()
}

/// Store, change, rename, move and delete are one command each, and each
/// inverse puts the list back as it was.
#[test]
fn scene_commands_have_exact_inverses() {
    let (mut p, ..) = project();
    let s1 = scene(&mut p, "Verse", false);
    let s2 = scene(&mut p, "Chorus", false);
    let before = p.clone();
    let add1 = Command::SetMixScene {
        scene: s1.id,
        value: Some(Box::new(s1.clone())),
        index: 0,
    };
    assert_eq!(add1.impact(), Impact::None);
    let undo1 = add1.apply(&mut p).unwrap();
    let undo2 = Command::SetMixScene {
        scene: s2.id,
        value: Some(Box::new(s2.clone())),
        index: 9,
    }
    .apply(&mut p)
    .unwrap();
    let names = |p: &Project| -> Vec<String> {
        p.mix_scenes.scenes.iter().map(|s| s.name.clone()).collect()
    };
    assert_eq!(names(&p), ["Verse", "Chorus"]);
    // Rename and move to the front in one replacement.
    let mut renamed = s2.clone();
    renamed.name = "Big Chorus".into();
    let with_two = p.clone();
    let undo_move = Command::SetMixScene {
        scene: s2.id,
        value: Some(Box::new(renamed)),
        index: 0,
    }
    .apply(&mut p)
    .unwrap();
    assert_eq!(names(&p), ["Big Chorus", "Verse"]);
    undo_move.apply(&mut p).unwrap();
    assert_eq!(p, with_two);
    // Delete with A and B naming it: both cleared, both back on undo.
    Command::SetMixSceneOptions {
        fade: SceneFade::default(),
        a: Some(s1.id),
        b: Some(s2.id),
    }
    .apply(&mut p)
    .unwrap();
    let named = p.clone();
    let undo_delete = Command::SetMixScene {
        scene: s1.id,
        value: None,
        index: 0,
    }
    .apply(&mut p)
    .unwrap();
    assert_eq!(names(&p), ["Chorus"]);
    assert_eq!((p.mix_scenes.a, p.mix_scenes.b), (None, Some(s2.id)));
    undo_delete.apply(&mut p).unwrap();
    assert_eq!(p, named);
    // Back to nothing.
    Command::SetMixSceneOptions {
        fade: SceneFade::default(),
        a: None,
        b: None,
    }
    .apply(&mut p)
    .unwrap();
    undo2.apply(&mut p).unwrap();
    undo1.apply(&mut p).unwrap();
    assert_eq!(p, before);
    // A replacement must keep its id.
    let wrong = Command::SetMixScene {
        scene: MixSceneId(5),
        value: Some(Box::new(s1)),
        index: 0,
    };
    assert!(wrong.apply(&mut p).is_err());
}

/// Files from before mix scenes load without them; scenes and a song's
/// scene round-trip; nothing is written while there are none.
#[test]
fn project_files_keep_their_scenes() {
    let (mut p, ..) = project();
    let json = serde_json::to_string(&p).unwrap();
    assert!(!json.contains("mix_scenes"), "nothing written when empty");
    let back: Project = serde_json::from_str(&json).unwrap();
    assert!(back.mix_scenes.is_empty());
    // An older file: no field at all.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value.as_object_mut().unwrap().remove("mix_scenes");
    let old: Project = serde_json::from_value(value).unwrap();
    assert_eq!(old.mix_scenes, MixScenes::default());
    let s = scene(&mut p, "Verse", true);
    let id = s.id;
    p.mix_scenes.scenes.push(s);
    p.mix_scenes.fade = SceneFade {
        length: FadeLength::Beats { beats: 4.0 },
        switch: SwitchPoint::End,
    };
    p.mix_scenes.a = Some(id);
    p.setlist.songs.push(crate::setlist::SetSong {
        name: "One".into(),
        start: faderframe_timeline::MusicalTime::ZERO,
        end: faderframe_timeline::MusicalTime(9600),
        then: Default::default(),
        notes: String::new(),
        scene: Some(id),
        gear: Default::default(),
    });
    let json = serde_json::to_string(&p).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    // Repair reserves the scenes' ids and drops repeated ones.
    let mut twice = p.clone();
    let copy = twice.mix_scenes.scenes[0].clone();
    twice.mix_scenes.scenes.push(copy);
    twice.ids = Default::default();
    let notes = twice.repair();
    assert_eq!(twice.mix_scenes.scenes.len(), 1);
    assert!(notes.iter().any(|n| n.contains("mix scenes")), "{notes:?}");
    let next: u64 = twice.ids.peek();
    assert!(next > id.raw());
}

/// A recall sets what the scene holds, as far as its scope reaches,
/// leaves safe tracks alone and passes over what is gone; applied, it
/// leaves nothing more to do.
#[test]
fn a_recall_sets_the_mix_within_its_scope() {
    let (mut p, a, b, aux) = project();
    let stored = scene(&mut p, "Verse", true);
    // The mix moves on.
    let moves = vec![
        Command::SetTrackVolume { track: a, db: -9.0 },
        Command::SetTrackPan { track: a, pan: 0.5 },
        Command::SetTrackMute { track: b, on: true },
        Command::SetTrackSolo { track: a, on: true },
        Command::SetTrackPhaseInvert { track: b, on: true },
        Command::SetSendLevel {
            track: a,
            send: SendId(900),
            db: -3.0,
        },
        Command::SetSendEnabled {
            track: a,
            send: SendId(900),
            enabled: false,
        },
        Command::SetPluginBypass {
            track: a,
            plugin: PluginInstanceId(901),
            bypass: true,
        },
        Command::SetPluginParameter {
            track: a,
            plugin: PluginInstanceId(901),
            parameter: ParameterId(0),
            value: Some(-6.0),
        },
        Command::SetTrackDelay { track: b, ms: 2.5 },
    ];
    apply_all(&mut p, moves);
    let moved = p.clone();
    // Everything back.
    let all = commands_to(
        &p,
        &stored.mix,
        &SceneScope::default(),
        &BTreeSet::new(),
        &explicit_value,
    );
    assert_eq!(all.len(), 10, "{all:#?}");
    let undo = apply_all(&mut p, all);
    let again = commands_to(
        &p,
        &stored.mix,
        &SceneScope::default(),
        &BTreeSet::new(),
        &explicit_value,
    );
    assert!(again.is_empty(), "{again:#?}");
    let t = |p: &Project, id| p.track(id).unwrap().clone();
    assert_eq!(t(&p, a).volume_db, 0.0);
    assert!(!t(&p, b).mute && !t(&p, a).solo && t(&p, a).sends[0].enabled);
    assert!(!t(&p, a).inserts[0].bypass);
    assert_eq!(t(&p, a).inserts[0].parameters[0].value, -20.0);
    // One undo step back to the moved mix.
    undo.apply(&mut p).unwrap();
    assert_eq!(p, moved);
    // Only faders and mutes.
    let scope = SceneScope::none()
        .with(ScopePart::Faders, true)
        .with(ScopePart::Mutes, true);
    let some = commands_to(&p, &stored.mix, &scope, &BTreeSet::new(), &explicit_value);
    assert!(
        some.iter().all(|c| matches!(
            c,
            Command::SetTrackVolume { .. } | Command::SetTrackMute { .. }
        )),
        "{some:#?}"
    );
    assert_eq!(some.len(), 2);
    // Gtr safe: nothing of it.
    let safe = BTreeSet::from([b]);
    let not_b = commands_to(
        &p,
        &stored.mix,
        &SceneScope::default(),
        &safe,
        &explicit_value,
    );
    assert!(not_b.iter().all(|c| !mentions(c, b)), "{not_b:#?}");
    assert_eq!(not_b.len(), 7);
    // Gone since: the aux track and the send to it.
    p.tracks.retain(|t| t.id != aux);
    p.track_mut(a).unwrap().sends.clear();
    let left = commands_to(
        &p,
        &stored.mix,
        &SceneScope::default(),
        &BTreeSet::new(),
        &explicit_value,
    );
    assert!(left.iter().all(|c| !matches!(
        c,
        Command::SetSendLevel { .. } | Command::SetSendEnabled { .. }
    )));
    apply_all(&mut p, left);
}

fn mentions(c: &Command, track: TrackId) -> bool {
    format!("{c:?}").contains(&format!("track: {track:?}"))
}

/// Nothing a command would refuse: a send is not switched on into a loop,
/// the master gets no delay, the drive needs a console.
#[test]
fn a_recall_asks_nothing_impossible() {
    let (mut p, a, _, aux) = project();
    let master = p.master_id().unwrap();
    p.track_mut(a).unwrap().sends[0].enabled = false;
    let mut stored = scene(&mut p, "S", false);
    stored.mix.tracks.get_mut(&a).unwrap().sends[0].enabled = true;
    stored.mix.tracks.get_mut(&master).unwrap().delay_ms = 5.0;
    stored.mix.console_drive = Some(4.0);
    // The aux now feeds the vocal: the send back would loop.
    p.track_mut(aux).unwrap().output = crate::OutputRouting::Track { track: a };
    let cmds = commands_to(
        &p,
        &stored.mix,
        &SceneScope::default(),
        &BTreeSet::new(),
        &explicit_value,
    );
    assert!(cmds.is_empty(), "{cmds:#?}");
}

/// Faders glide in fader travel, steadily, and end exactly on the scene;
/// switched values change at the switch point.
#[test]
fn a_blend_glides_and_switches() {
    let (mut p, a, b, _) = project();
    let from = scene(&mut p, "From", true).mix;
    let mut to = from.clone();
    {
        let t = to.tracks.get_mut(&a).unwrap();
        t.volume_db = -30.0;
        t.pan = -1.0;
        t.mute = true;
        t.sends[0].level_db = 0.0;
        t.devices[0].values.as_mut().unwrap().parameters[0].value = -40.0;
    }
    to.tracks.get_mut(&b).unwrap().pan_mode = PanMode::Dual;
    let glide = |_: PluginInstanceId, _: ParameterId| ParamGlide::Linear;
    let law = FaderLaw::console();
    let mut last_pos = f32::INFINITY;
    let mut last_pan = 1.0f32;
    for i in 0..=20 {
        let t = i as f64 / 20.0;
        let m = blend(&from, &to, t, SwitchPoint::Middle, &glide);
        let x = &m.tracks[&a];
        let pos = law.db_to_position(x.volume_db);
        assert!(pos <= last_pos + 1e-6, "the fader falls steadily");
        assert!(x.pan <= last_pan + 1e-6);
        last_pos = pos;
        last_pan = x.pan;
        assert_eq!(x.mute, t >= 0.5, "switched halfway");
        assert_eq!(m.tracks[&b].pan_mode == PanMode::Dual, t >= 0.5);
        let send = x.sends[0].level_db;
        assert!((-12.0..=0.0).contains(&send));
    }
    // Even steps of travel.
    let quarter = blend(&from, &to, 0.25, SwitchPoint::Middle, &glide);
    let expected = (law.db_to_position(0.0) * 3.0 + law.db_to_position(-30.0)) / 4.0;
    assert!((law.db_to_position(quarter.tracks[&a].volume_db) - expected).abs() < 1e-4);
    let half = blend(&from, &to, 0.5, SwitchPoint::Middle, &glide);
    let p0 = half.tracks[&a].devices[0]
        .values
        .as_ref()
        .unwrap()
        .parameters[0]
        .value;
    assert!((p0 + 30.0).abs() < 1e-9, "the parameter halfway: {p0}");
    assert_eq!(blend(&from, &to, 1.0, SwitchPoint::Middle, &glide), to);
    assert_eq!(blend(&from, &to, 0.0, SwitchPoint::End, &glide), from);
    // At the start: switched as soon as it moves; at the end: only there.
    let early = blend(&from, &to, 0.01, SwitchPoint::Start, &glide);
    assert!(early.tracks[&a].mute);
    let late = blend(&from, &to, 0.99, SwitchPoint::End, &glide);
    assert!(!late.tracks[&a].mute);
    // A stepped parameter switches.
    let stepped = |_: PluginInstanceId, _: ParameterId| ParamGlide::Switch;
    let m = blend(&from, &to, 0.4, SwitchPoint::Middle, &stepped);
    let v = m.tracks[&a].devices[0].values.as_ref().unwrap().parameters[0].value;
    assert_eq!(v, -20.0);
    // Frequencies glide geometrically.
    assert!((blend_param(ParamGlide::Log, 100.0, 10_000.0, 0.5, false) - 1000.0).abs() < 1e-6);
}

/// A track only one scene holds is in the blend on its side of the
/// switch point (so a crossfade into a scene stored before the track was
/// added leaves it alone).
#[test]
fn tracks_only_one_side_holds_follow_the_switch() {
    let (mut p, a, b, _) = project();
    let from = scene(&mut p, "From", false).mix;
    let mut to = from.clone();
    to.tracks.remove(&b);
    let glide = |_: PluginInstanceId, _: ParameterId| ParamGlide::Linear;
    assert!(
        blend(&from, &to, 0.2, SwitchPoint::Middle, &glide)
            .tracks
            .contains_key(&b)
    );
    assert!(
        !blend(&from, &to, 0.7, SwitchPoint::Middle, &glide)
            .tracks
            .contains_key(&b)
    );
    assert!(
        blend(&from, &to, 0.7, SwitchPoint::Middle, &glide)
            .tracks
            .contains_key(&a)
    );
}

#[test]
fn fade_lengths_read_and_print() {
    assert_eq!(FadeLength::parse("instant"), Some(FadeLength::Instant));
    assert_eq!(FadeLength::parse("0"), Some(FadeLength::Instant));
    assert_eq!(
        FadeLength::parse("350 ms"),
        Some(FadeLength::Millis { ms: 350.0 })
    );
    assert_eq!(
        FadeLength::parse("2,5"),
        Some(FadeLength::Millis { ms: 2500.0 })
    );
    assert_eq!(
        FadeLength::parse("4 beats"),
        Some(FadeLength::Beats { beats: 4.0 })
    );
    assert_eq!(FadeLength::parse("loud"), None);
    assert_eq!(FadeLength::parse("-3 s"), None);
    assert_eq!(FadeLength::Millis { ms: 350.0 }.label(), "350 ms");
    assert_eq!(FadeLength::Millis { ms: 2000.0 }.label(), "2 s");
    assert_eq!(FadeLength::Millis { ms: 2500.0 }.label(), "2.5 s");
    assert_eq!(FadeLength::Beats { beats: 1.0 }.label(), "1 beat");
    assert_eq!(FadeLength::Beats { beats: 4.0 }.seconds(120.0), 2.0);
    assert_eq!(
        FadeLength::Millis { ms: 1e9 }.clamped(),
        FadeLength::Millis { ms: MAX_FADE_MS }
    );
    assert!(SceneScope::default().has(ScopePart::Plugins));
    let s = SceneScope::default().with(ScopePart::Sends, false);
    assert!(!s.has(ScopePart::Sends) && !s.and(SceneScope::default()).has(ScopePart::Sends));
}
