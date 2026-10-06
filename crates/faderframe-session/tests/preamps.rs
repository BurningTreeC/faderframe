#![allow(clippy::unwrap_used)]
use faderframe_core::{ParameterId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef, Project, TrackKind, preset::TrackPreset};
use faderframe_session::{Action, Session};

#[test]
fn dedicated_preamp_is_persistent_undoable_and_not_an_insert_choice() {
    let mut s = Session::new(
        Project::new("Preamp", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let track = s.add_track(TrackKind::Audio).unwrap();
    assert!(
        s.available_plugins()
            .iter()
            .all(|p| builtin::preamp_index(&p.plugin.id).is_none())
    );
    s.dispatch(Action::SetPreamp {
        track,
        model: Some(1),
    })
    .unwrap();
    let slot = s.project().track(track).unwrap().preamp.clone().unwrap();
    assert!(s.project().track(track).unwrap().inserts.is_empty());
    assert_eq!(s.plugin_slot(slot.id).unwrap().0.id, track);
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track,
        plugin: slot.id,
        parameter: ParameterId(0),
        value: Some(0.75),
    }))
    .unwrap();
    assert_eq!(
        s.static_value(
            track,
            faderframe_automation::AutomationTarget::PluginParameter {
                plugin: slot.id,
                parameter: ParameterId(0)
            }
        ),
        Some(0.75)
    );
    assert!(
        s.automatable_parameters(track)
            .iter()
            .any(|p| p.name.contains("Gain"))
    );
    let json = faderframe_project::file::to_string(s.project(), None).unwrap();
    let p = faderframe_project::file::from_str(&json).unwrap().project;
    assert_eq!(
        p.track(track).unwrap().preamp,
        s.project().track(track).unwrap().preamp
    );
    s.dispatch(Action::SetPreamp {
        track,
        model: Some(2),
    })
    .unwrap();
    assert_ne!(
        s.project()
            .track(track)
            .unwrap()
            .preamp
            .as_ref()
            .unwrap()
            .id,
        slot.id
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .track(track)
            .unwrap()
            .preamp
            .as_ref()
            .unwrap()
            .id,
        slot.id
    );
    assert_eq!(
        s.plugin_parameter_value(slot.id, ParameterId(0)),
        Some(0.75)
    );
    s.dispatch(Action::SetPreamp { track, model: None })
        .unwrap();
    assert!(s.project().track(track).unwrap().preamp.is_none());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .track(track)
            .unwrap()
            .preamp
            .as_ref()
            .unwrap()
            .id,
        slot.id
    );
    let midi = s.add_track(TrackKind::Midi).unwrap();
    assert!(
        s.dispatch(Action::SetPreamp {
            track: midi,
            model: Some(0)
        })
        .is_err()
    );
    assert!(
        s.dispatch(Action::SetPreamp {
            track,
            model: Some(99)
        })
        .is_err()
    );
    assert!(
        s.dispatch(Action::InsertPlugin {
            track,
            index: 0,
            plugin: PluginRef::builtin(builtin::PREAMPS[0].0, "Preamp")
        })
        .is_err()
    );
}

#[test]
fn old_projects_default_to_no_preamp_and_track_presets_keep_it() {
    let mut p = Project::new("Old", 48_000);
    let track = p.master_id().unwrap();
    let mut s = Session::new(p.clone(), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::SetPreamp {
        track,
        model: Some(4),
    })
    .unwrap();
    let preset = TrackPreset::capture(s.project(), s.project().track(track).unwrap());
    let (copy, _) = preset.instantiate(&mut p, Some("Copy"));
    assert_eq!(
        copy.preamp.as_ref().unwrap().plugin.id,
        builtin::PREAMPS[4].0
    );
    let mut json = serde_json::to_value(Project::new("Old", 48_000)).unwrap();
    for t in json["tracks"].as_array_mut().unwrap() {
        t.as_object_mut().unwrap().remove("preamp");
    }
    let restored: Project = serde_json::from_value(json).unwrap();
    assert!(restored.tracks.iter().all(|t| t.preamp.is_none()));
}

#[test]
fn synth_preamp_master_changes_playing_audio_live_and_rendered_ahead() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_session::{AudioPreferences, TransportAction};
    use std::time::{Duration, Instant};
    let run = |s: &mut Session, millis| {
        let start = Instant::now();
        let mut previous = start;
        while start.elapsed() < Duration::from_millis(millis) {
            std::thread::sleep(Duration::from_millis(20));
            // Hosted runners may sleep much longer than requested. Meter
            // ballistics must see the elapsed time, just as in the GUI.
            let now = Instant::now();
            s.tick(now.duration_since(previous).as_secs_f32());
            previous = now;
        }
    };
    for ahead in [false, true] {
        let mut project = faderframe_project::demo::demo_project(48_000);
        let synth_id = project.ids.allocate();
        let t = project
            .tracks
            .iter_mut()
            .find(|t| t.instrument.is_some())
            .unwrap();
        let track = t.id;
        // Older projects may retain an invisible instrument beneath the
        // visible synth insert. The preamp must process the latter's audio.
        let mut synth = t.instrument.clone().unwrap();
        synth.id = synth_id;
        t.inserts.push(synth);
        t.sends.clear();
        let mut s = Session::new(project, None, EngineConfig::default()).unwrap();
        s.set_render_ahead(ahead.then(|| Duration::from_millis(150)))
            .unwrap();
        s.start_audio(
            vec![Box::new(DummyBackend::default())],
            &AudioPreferences::default(),
        )
        .unwrap();
        s.dispatch(Action::Transport(TransportAction::Locate(
            faderframe_timeline::MusicalTime::from_quarters_i(16),
        )))
        .unwrap();
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        run(&mut s, 800);
        // Insert the preamp while the existing synth processor is playing.
        s.dispatch(Action::SetPreamp {
            track,
            model: Some(1),
        })
        .unwrap();
        run(&mut s, 800);
        assert_eq!(s.render_ahead_status().0, usize::from(ahead));
        let loud = s.meter(track).left.level_db;
        let plugin = s
            .project()
            .track(track)
            .unwrap()
            .preamp
            .as_ref()
            .unwrap()
            .id;
        s.dispatch(Action::Edit(Command::SetPluginParameter {
            track,
            plugin,
            parameter: ParameterId(1),
            value: Some(-60.0),
        }))
        .unwrap();
        // Allow the meter's peak release to reach the attenuated level.
        run(&mut s, 2600);
        let quiet = s.meter(track).left.level_db;
        assert!(loud > -50.0, "ahead={ahead}: synth is audible ({loud})");
        assert!(
            quiet <= (loud - 40.0).max(faderframe_session::METER_FLOOR_DB),
            "ahead={ahead}: Master must attenuate the synth: {loud} -> {quiet}"
        );
        s.stop_audio();
    }
}
