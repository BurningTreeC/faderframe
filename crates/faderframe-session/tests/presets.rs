//! Plugin presets: saving and loading (undoable), listing.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ParameterId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef};
use faderframe_session::{Action, Session};

#[test]
fn presets_save_load_and_undo() {
    let data = std::env::temp_dir().join(format!("ff-presets-{}", std::process::id()));
    // SAFETY: set before any thread of this test binary reads it (one test).
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index: 0,
        plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
    })
    .unwrap();
    let echo = s.project().track(bass).unwrap().inserts[0].id;
    let p = ParameterId(1);
    let set = |s: &mut Session, v: f64| {
        s.dispatch(Action::Edit(Command::SetPluginParameter {
            track: bass,
            plugin: echo,
            parameter: p,
            value: Some(v),
        }))
        .unwrap();
    };
    let value = |s: &Session| {
        s.plugin_slot(echo)
            .unwrap()
            .1
            .parameters
            .iter()
            .find(|q| q.id == p)
            .map(|q| q.value)
    };
    set(&mut s, 0.37);
    s.dispatch(Action::SavePluginPreset {
        plugin: echo,
        name: "Short Slap".into(),
    })
    .unwrap();
    let presets = s.plugin_presets(echo);
    assert_eq!(presets.len(), 1);
    assert_eq!(presets[0].name, "Short Slap");
    assert!(presets[0].path.starts_with(&data));
    set(&mut s, 0.9);
    s.dispatch(Action::LoadPluginPreset {
        plugin: echo,
        path: presets[0].path.clone(),
    })
    .unwrap();
    assert!((value(&s).unwrap() - 0.37).abs() < 1e-6, "{:?}", value(&s));
    assert_eq!(
        s.plugin_parameter_value(echo, p)
            .map(|v| (v * 100.0).round() / 100.0),
        Some(0.37),
        "the running plugin follows"
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(value(&s), Some(0.9));
    // A preset of another plugin is refused.
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index: 1,
        plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
    })
    .unwrap();
    let gain = s.project().track(bass).unwrap().inserts[1].id;
    assert!(
        s.dispatch(Action::LoadPluginPreset {
            plugin: gain,
            path: presets[0].path.clone(),
        })
        .is_err()
    );
    // Saving under a name that exists (in any case) replaces that preset;
    // the shell asks first by the same check.
    let clash = s.existing_plugin_preset(echo, "short slap").unwrap();
    assert_eq!(clash.name, "Short Slap");
    assert!(s.existing_plugin_preset(echo, "Long Slap").is_none());
    set(&mut s, 0.5);
    s.dispatch(Action::SavePluginPreset {
        plugin: echo,
        name: "short slap".into(),
    })
    .unwrap();
    let presets = s.plugin_presets(echo);
    assert_eq!(presets.len(), 1, "replaced, not doubled");
    assert_eq!(presets[0].name, "short slap");
    // Only the user's own presets can be deleted.
    let outside = data.join("not-a-preset.ffpreset");
    std::fs::write(&outside, b"{}").unwrap();
    assert!(
        s.dispatch(Action::DeletePreset {
            path: outside.clone()
        })
        .is_err()
    );
    let sneaky = presets[0]
        .path
        .parent()
        .unwrap()
        .join("../../../../not-a-preset.ffpreset");
    assert!(s.dispatch(Action::DeletePreset { path: sneaky }).is_err());
    assert!(outside.exists());
    s.dispatch(Action::DeletePreset {
        path: presets[0].path.clone(),
    })
    .unwrap();
    assert!(s.plugin_presets(echo).is_empty());
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn track_presets_are_replaced_only_when_asked_and_can_be_deleted() {
    let dir = std::env::temp_dir().join(format!("ff-track-presets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_track_preset_dir(dir.clone());
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let save = |s: &mut Session, name: &str, replace: bool| {
        s.dispatch(Action::SaveTrackPreset {
            track: bass,
            name: Some(name.into()),
            replace,
        })
        .unwrap();
    };
    save(&mut s, "Fat Bass", false);
    assert!(s.existing_track_preset("fat bass").is_some());
    assert!(s.existing_track_preset("Thin Bass").is_none());
    // Keep both: the new one numbered.
    save(&mut s, "Fat Bass", false);
    let names: Vec<_> = s.track_presets().iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["Fat Bass", "Fat Bass 2"]);
    // Replace: one file, under the new spelling.
    save(&mut s, "FAT BASS", true);
    let names: Vec<_> = s.track_presets().iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["FAT BASS", "Fat Bass 2"]);
    let path = s.track_presets()[0].path.clone();
    s.dispatch(Action::DeletePreset { path }).unwrap();
    let names: Vec<_> = s.track_presets().iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["Fat Bass 2"]);
    // A file of another kind in the folder stays.
    let other = dir.join("notes.txt");
    std::fs::write(&other, b"x").unwrap();
    assert!(
        s.dispatch(Action::DeletePreset {
            path: other.clone()
        })
        .is_err()
    );
    assert!(other.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
