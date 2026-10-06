//! Project versions: saved, compared and restored.
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::Command;
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
fn versions_are_saved_compared_and_restored() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    // Not before the project is saved.
    assert!(
        s.dispatch(Action::SaveVersion {
            name: "Too soon".into()
        })
        .is_err()
    );
    let dir = std::env::temp_dir().join(format!("ff-versions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    s.save_as(&dir.join("song.ffproj")).unwrap();
    s.dispatch(Action::SaveVersion {
        name: "First mix".into(),
    })
    .unwrap();
    let bass = track(&s, "Bass");
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: bass,
        db: -12.0,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: bass,
        name: "Sub".into(),
    }))
    .unwrap();
    let versions = s.versions();
    assert_eq!(versions.len(), 1);
    assert_eq!(
        (versions[0].number, versions[0].name.as_str()),
        (1, "First mix")
    );
    // What changed since.
    let changes = s.compare_version(&versions[0].path).unwrap();
    assert!(
        changes.iter().any(|c| c.starts_with("‘Sub’: volume")),
        "{changes:#?}"
    );
    assert!(changes.iter().any(|c| c == "‘Sub’: renamed from ‘Bass’"));
    // Restored: as it was, the present kept as a version, unsaved.
    s.dispatch(Action::RestoreVersion(versions[0].path.clone()))
        .unwrap();
    let t = s.project().track(bass).unwrap();
    assert_eq!((t.name.as_str(), t.volume_db), ("Bass", -3.0));
    assert!(s.is_dirty());
    let versions = s.versions();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[1].name, "Before restoring 1");
    // That one brings the change back.
    s.dispatch(Action::RestoreVersion(versions[1].path.clone()))
        .unwrap();
    assert_eq!(s.project().track(bass).unwrap().name, "Sub");
    let _ = std::fs::remove_dir_all(&dir);
}
