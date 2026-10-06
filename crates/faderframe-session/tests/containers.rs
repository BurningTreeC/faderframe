//! Containers through the session: a new one has its chains, devices go
//! into chains and are the track's like any other, chains change with
//! undo, and the project keeps them.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ParameterId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef};
use faderframe_session::{Action, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn chain_names(s: &Session, t: TrackId, c: faderframe_core::PluginInstanceId) -> Vec<String> {
    s.container_chains(t, c)
        .unwrap()
        .iter()
        .map(|c| c.name.clone())
        .collect()
}

#[test]
fn a_container_holds_devices_in_chains() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let before = s.project().track(bass).unwrap().inserts.len();
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index: before,
        plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
    })
    .unwrap();
    let container = s.project().track(bass).unwrap().inserts[before].id;
    assert_eq!(chain_names(&s, bass, container), ["Dry", "Chain 2"]);
    // A utility into the second chain: the track's like any device.
    s.dispatch(Action::InsertIntoChain {
        track: bass,
        container,
        chain: 1,
        index: 0,
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
    })
    .unwrap();
    let utility = s.container_chains(bass, container).unwrap()[1].inserts[0].id;
    assert_eq!(s.plugin_slot(utility).unwrap().0.id, bass);
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: bass,
        plugin: utility,
        parameter: ParameterId(0),
        value: Some(-9.0),
    }))
    .unwrap();
    assert_eq!(
        s.plugin_parameter_value(utility, ParameterId(0)),
        Some(-9.0)
    );
    // Chains come and go, with undo.
    s.dispatch(Action::AddChain {
        track: bass,
        container,
    })
    .unwrap();
    s.dispatch(Action::RenameChain {
        track: bass,
        container,
        chain: 2,
        name: "Crush".into(),
    })
    .unwrap();
    assert_eq!(
        chain_names(&s, bass, container),
        ["Dry", "Chain 2", "Crush"]
    );
    s.dispatch(Action::RemoveChain {
        track: bass,
        container,
        chain: 0,
    })
    .unwrap();
    assert_eq!(chain_names(&s, bass, container), ["Chain 2", "Crush"]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        chain_names(&s, bass, container),
        ["Dry", "Chain 2", "Crush"]
    );
    // A chain's mix, one step a drag.
    s.dispatch(Action::BeginGesture("Chain Level".into()))
        .unwrap();
    for db in [-1.0, -2.0, -3.0] {
        s.dispatch(Action::Edit(Command::SetChainMix {
            track: bass,
            container,
            chain: 1,
            gain_db: db,
            pan: 0.0,
            mute: false,
            solo: false,
        }))
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(
        s.container_chains(bass, container).unwrap()[1].gain_db,
        -3.0
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.container_chains(bass, container).unwrap()[1].gain_db, 0.0);
    // Out of the chain again.
    s.dispatch(Action::RemoveFromChain {
        track: bass,
        plugin: utility,
    })
    .unwrap();
    assert!(s.plugin_slot(utility).is_none());
    // MIDI effects stay before the instrument.
    assert!(
        s.dispatch(Action::InsertIntoChain {
            track: bass,
            container,
            chain: 0,
            index: 0,
            plugin: PluginRef::builtin(builtin::ARPEGGIATOR, "Arpeggiator"),
        })
        .is_err()
    );
}

#[test]
fn the_project_keeps_its_containers() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index: 0,
        plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
    })
    .unwrap();
    let container = s.project().track(bass).unwrap().inserts[0].id;
    s.dispatch(Action::InsertIntoChain {
        track: bass,
        container,
        chain: 1,
        index: 0,
        plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
    })
    .unwrap();
    let dir = std::env::temp_dir().join(format!("ff-containers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.ffproj");
    faderframe_project::file::save(&path, s.project(), None).unwrap();
    let loaded = faderframe_project::file::load(&path).unwrap().project;
    let t = loaded.track(bass).unwrap();
    assert_eq!(t.containers, s.project().track(bass).unwrap().containers);
    assert_eq!(t.containers.len(), 2, "the nested one too");
    std::fs::remove_dir_all(&dir).ok();
}
