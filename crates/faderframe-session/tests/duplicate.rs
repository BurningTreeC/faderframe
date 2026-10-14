//! Duplicate Track (with or without its content): fresh ids for the
//! track, its devices, sends, lanes and modulators; the copy right after
//! the original; one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_automation::{AutomationCurve, AutomationLane, AutomationTarget};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, TrackKind};
use faderframe_session::{Action, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn names(s: &Session) -> Vec<String> {
    s.project().tracks.iter().map(|t| t.name.clone()).collect()
}

#[test]
fn a_duplicate_has_its_own_devices_lanes_and_clips() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    // An effect, and a lane automating one of its parameters.
    let index = s.project().track(bass).unwrap().inserts.len();
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index,
        plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::GAIN, "Utility"),
    })
    .unwrap();
    let fx: PluginInstanceId = s.project().track(bass).unwrap().inserts.last().unwrap().id;
    let id = faderframe_core::AutomationLaneId(4242);
    s.dispatch(Action::Edit(Command::AddAutomationLane {
        track: bass,
        lane: Box::new(AutomationLane {
            id,
            target: AutomationTarget::PluginParameter {
                plugin: fx,
                parameter: faderframe_core::ParameterId(0),
            },
            curve: AutomationCurve::default(),
            mode: Default::default(),
            visible: true,
        }),
    }))
    .unwrap();
    let before = names(&s);
    let original = s.project().track(bass).unwrap().clone();
    assert!(!original.clips.is_empty());

    s.dispatch(Action::DuplicateTracks {
        tracks: vec![bass],
        content: true,
    })
    .unwrap();
    let p = s.project();
    let at = p.track_index(bass).unwrap();
    let copy = &p.tracks[at + 1];
    assert_eq!(copy.name, "Bass 2");
    assert_ne!(copy.id, bass);
    assert_eq!(copy.kind, original.kind);
    assert_eq!(copy.inserts.len(), original.inserts.len());
    for (a, b) in original.inserts.iter().zip(&copy.inserts) {
        assert_ne!(a.id, b.id, "a device of its own");
        assert_eq!(a.plugin, b.plugin);
    }
    let copied_fx = copy.inserts.last().unwrap().id;
    assert!(
        copy.automation.lanes.iter().any(|l| l.target
            == AutomationTarget::PluginParameter {
                plugin: copied_fx,
                parameter: faderframe_core::ParameterId(0),
            }),
        "the lane follows the copied device"
    );
    assert_eq!(copy.clips.len(), original.clips.len());
    for (a, b) in original.clips.iter().zip(&copy.clips) {
        assert_ne!(a, b);
        let (ca, cb) = (p.clip(*a).unwrap(), p.clip(*b).unwrap());
        assert_eq!(cb.track, copy.id);
        assert_eq!(ca.start, cb.start);
        assert_eq!(ca.content, cb.content);
    }
    assert!(s.selection.tracks.contains(&copy.id));
    let copy_id = copy.id;
    // Undo takes it all away in one step.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(names(&s), before);
    assert!(s.project().track(copy_id).is_none());

    // Without content: no clips.
    s.dispatch(Action::DuplicateTracks {
        tracks: vec![bass],
        content: false,
    })
    .unwrap();
    let p = s.project();
    let copy = &p.tracks[p.track_index(bass).unwrap() + 1];
    assert_eq!(copy.name, "Bass 2");
    assert!(copy.clips.is_empty());
    assert_eq!(copy.inserts.len(), original.inserts.len());
}

#[test]
fn a_folder_comes_with_copies_of_its_tracks() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let folder = track(&s, "Arpeggios");
    let inside: Vec<TrackId> = s
        .project()
        .tracks
        .iter()
        .filter(|t| t.folder == Some(folder))
        .map(|t| t.id)
        .collect();
    assert!(!inside.is_empty());
    let count = s.project().tracks.len();
    s.dispatch(Action::DuplicateTracks {
        tracks: vec![folder],
        content: true,
    })
    .unwrap();
    let p = s.project();
    assert_eq!(p.tracks.len(), count + 1 + inside.len());
    let copy = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Folder && t.id != folder)
        .unwrap();
    let held: Vec<&str> = p
        .tracks
        .iter()
        .filter(|t| t.folder == Some(copy.id))
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(held.len(), inside.len(), "{held:?}");
    // In folder order: the original folder and its tracks, then the copy
    // and its tracks.
    let order: Vec<TrackId> = p.folder_order().iter().map(|t| t.id).collect();
    let f = order.iter().position(|t| *t == folder).unwrap();
    let c = order.iter().position(|t| *t == copy.id).unwrap();
    assert_eq!(
        c,
        f + 1 + inside.len(),
        "the copy follows the original's contents"
    );
}
