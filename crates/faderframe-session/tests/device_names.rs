//! Several devices of one name on a track: the second is "Delay (2)", the
//! third "Delay (3)" — in automation, modulation and editor names alike.
#![allow(clippy::unwrap_used)]

use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::PluginRef;
use faderframe_session::{Action, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

#[test]
fn devices_of_one_name_are_numbered_everywhere() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    for _ in 0..3 {
        let index = s.project().track(bass).unwrap().inserts.len();
        s.dispatch(Action::InsertPlugin {
            track: bass,
            index,
            plugin: PluginRef::builtin(builtin::ECHO, "Delay"),
        })
        .unwrap();
    }
    let t = s.project().track(bass).unwrap().clone();
    let delays: Vec<_> = t
        .inserts
        .iter()
        .filter(|x| x.plugin.id == builtin::ECHO)
        .collect();
    assert_eq!(delays.len(), 3);
    let names: Vec<String> = delays.iter().map(|d| t.name_of(d)).collect();
    assert_eq!(names, ["Delay", "Delay (2)", "Delay (3)"]);
    assert_eq!(s.device_name(delays[1].id).as_deref(), Some("Delay (2)"));
    // Automation lanes name the device the same way.
    let params = s.automatable_parameters(bass);
    for n in &names {
        let prefix = format!("{n}: ");
        assert!(
            params.iter().any(|p| p.name.starts_with(&prefix)),
            "a lane of {n}"
        );
    }
    // Removing the first renumbers: what was "Delay (2)" is now "Delay".
    s.dispatch(Action::Edit(faderframe_project::Command::RemovePlugin {
        track: bass,
        plugin: delays[0].id,
    }))
    .unwrap();
    assert_eq!(s.device_name(delays[1].id).as_deref(), Some("Delay"));
    assert_eq!(s.device_name(delays[2].id).as_deref(), Some("Delay (2)"));
}
