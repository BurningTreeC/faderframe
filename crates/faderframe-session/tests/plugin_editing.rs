#![allow(clippy::unwrap_used)]
//! Editing plugin parameters from a (generic) editor: values reach the
//! hosted instance, undo restores the plugin's own value, a gesture is one
//! undo step, and editors are requested through the shell.

use faderframe_core::{ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef, Project, TrackKind};
use faderframe_session::{Action, PluginTarget, Session, UiRequest};

const FEEDBACK: ParameterId = ParameterId(1);

/// Built-ins keep parameters in f32.
#[track_caller]
fn assert_value(s: &mut Session, plugin: PluginInstanceId, want: f64) {
    let v = s.plugin_parameter_value(plugin, FEEDBACK).unwrap();
    assert!((v - want).abs() < 1e-6, "{v} != {want}");
}

fn session_with_echo() -> (Session, TrackId, PluginInstanceId) {
    let mut s = Session::new(Project::new("Edit", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
    })
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    (s, t, plugin)
}

fn set(s: &mut Session, track: TrackId, plugin: PluginInstanceId, v: f64) {
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track,
        plugin,
        parameter: FEEDBACK,
        value: Some(v),
    }))
    .unwrap();
}

#[test]
fn parameter_edits_reach_the_plugin_and_undo_restores_its_value() {
    let (mut s, t, plugin) = session_with_echo();
    let original = s.plugin_parameter_value(plugin, FEEDBACK).unwrap();
    let views = s.plugin_parameter_views(plugin);
    let fb = views.iter().find(|v| v.info.id == FEEDBACK).unwrap();
    assert_eq!(fb.value, original);
    assert!(!fb.explicit);

    set(&mut s, t, plugin, 0.8);
    assert_value(&mut s, plugin, 0.8);
    let slot = &s.project().track(t).unwrap().inserts[0];
    assert_eq!(slot.parameters.len(), 1);
    assert!(
        s.plugin_parameter_views(plugin)
            .iter()
            .any(|v| v.info.id == FEEDBACK && v.explicit)
    );

    s.dispatch(Action::Undo).unwrap();
    assert!(
        s.project().track(t).unwrap().inserts[0]
            .parameters
            .is_empty()
    );
    assert_value(&mut s, plugin, original);
    s.dispatch(Action::Redo).unwrap();
    assert_value(&mut s, plugin, 0.8);
}

#[test]
fn a_gesture_is_one_undo_step() {
    let (mut s, t, plugin) = session_with_echo();
    let original = s.plugin_parameter_value(plugin, FEEDBACK).unwrap();
    s.dispatch(Action::BeginGesture("Change Feedback".into()))
        .unwrap();
    for v in [0.1, 0.2, 0.3, 0.4] {
        set(&mut s, t, plugin, v);
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_value(&mut s, plugin, 0.4);
    s.dispatch(Action::Undo).unwrap();
    assert_value(&mut s, plugin, original);
}

#[test]
fn values_out_of_range_are_clamped_by_the_plugin() {
    let (mut s, t, plugin) = session_with_echo();
    set(&mut s, t, plugin, 5.0);
    // The Delay's feedback goes to 110 %.
    assert_value(&mut s, plugin, 1.1);
}

#[test]
fn editors_are_requested_from_the_shell() {
    let (mut s, t, plugin) = session_with_echo();
    s.take_ui_requests();
    s.dispatch(Action::OpenPluginEditor {
        track: t,
        plugin,
        generic: true,
    })
    .unwrap();
    assert_eq!(
        s.take_ui_requests(),
        vec![UiRequest::PluginEditor {
            track: t,
            plugin,
            generic: true
        }]
    );
    // Built-ins without an editor of their own do not pop up on insertion.
    assert!(s.plugin_editor(plugin).is_none());
    s.place_plugin(
        t,
        PluginTarget::Insert(1),
        PluginRef::builtin(builtin::LATENCY_PROBE, "Latency Probe"),
    )
    .unwrap();
    assert!(s.take_ui_requests().is_empty());
    let (track, slot) = s.plugin_slot(plugin).unwrap();
    assert_eq!((track.id, slot.plugin.id.as_str()), (t, builtin::ECHO));
}

#[test]
fn editor_positions_are_saved_with_the_project() {
    let (mut s, _, plugin) = session_with_echo();
    s.dispatch(Action::SetPluginWindowPosition {
        plugin,
        x: 1335,
        y: 810,
    })
    .unwrap();
    let dir = std::env::temp_dir().join(format!("ff-session-editors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Editors.ffproj");
    s.save_as(&path).unwrap();
    let mut reopened =
        Session::new(Project::new("Other", 48_000), None, EngineConfig::default()).unwrap();
    reopened.open(&path).unwrap();
    assert_eq!(
        reopened.workspace().plugin_windows.get(&plugin),
        Some(&(1335, 810))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
