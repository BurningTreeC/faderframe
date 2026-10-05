#![allow(clippy::unwrap_used)]

use faderframe_core::builtin;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef, Project, TrackKind};
use faderframe_session::{Action, PluginTarget, Session, UiRequest};

#[test]
fn chooser_adds_a_visible_instrument_and_replacement_preserves_effects_and_undo() {
    let mut s = Session::new(
        Project::new("Instruments", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let track = s.add_track(TrackKind::Instrument).unwrap();
    s.take_ui_requests();
    s.place_plugin(
        track,
        PluginTarget::Instrument,
        PluginRef::builtin(builtin::SYNTH, "Synth"),
    )
    .unwrap();
    let t = s.project().track(track).unwrap();
    assert!(t.instrument.is_none());
    assert_eq!(t.inserts.len(), 1);
    let synth = t.inserts[0].id;
    assert_eq!(s.instrument_slot(t).unwrap().id, synth);
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|r| matches!(r, UiRequest::PluginEditor { plugin, .. } if *plugin == synth))
    );
    s.place_plugin(
        track,
        PluginTarget::Insert(1),
        PluginRef::builtin(builtin::ECHO, "Echo"),
    )
    .unwrap();
    let echo = s.project().track(track).unwrap().inserts[1].clone();
    s.place_plugin(
        track,
        PluginTarget::Instrument,
        PluginRef::builtin(builtin::DRUMS, "Drums"),
    )
    .unwrap();
    let t = s.project().track(track).unwrap();
    assert_eq!(t.inserts.len(), 2);
    assert_eq!(t.inserts[0].plugin.id, builtin::DRUMS);
    assert_eq!(t.inserts[1], echo);
    s.dispatch(Action::Undo).unwrap();
    let t = s.project().track(track).unwrap();
    assert_eq!(t.inserts[0].id, synth);
    assert_eq!(t.inserts[1], echo);
    // Loading the saved chain keeps its visible instrument identification.
    let json = faderframe_project::file::to_string(s.project(), None).unwrap();
    let p = faderframe_project::file::from_str(&json).unwrap().project;
    let reopened = Session::new(p, None, EngineConfig::default()).unwrap();
    assert_eq!(
        reopened
            .instrument_slot(reopened.project().track(track).unwrap())
            .unwrap()
            .id,
        synth
    );
    // The explicit insert target keeps its chosen position, even for synths.
    s.place_plugin(
        track,
        PluginTarget::Insert(2),
        PluginRef::builtin(builtin::SYNTH, "Synth"),
    )
    .unwrap();
    assert_eq!(
        s.project().track(track).unwrap().inserts[2].plugin.id,
        builtin::SYNTH
    );
    s.dispatch(Action::Edit(Command::RemovePlugin {
        track,
        plugin: synth,
    }))
    .unwrap();
    assert_eq!(
        s.instrument_slot(s.project().track(track).unwrap())
            .unwrap()
            .id,
        s.project().track(track).unwrap().inserts[1].id
    );
}
