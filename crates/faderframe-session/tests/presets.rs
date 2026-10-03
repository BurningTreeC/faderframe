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
    let _ = std::fs::remove_dir_all(&data);
}
