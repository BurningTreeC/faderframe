#![allow(clippy::unwrap_used)]
//! Track presets through the session: save into the library, list, recall
//! as a new track, apply onto another track, undo.

use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::{Action, Session};

#[test]
fn save_list_recall_and_apply_track_presets() {
    let dir = std::env::temp_dir().join(format!("ff-presets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::new(Project::new("P", 48_000), None, EngineConfig::default()).unwrap();
    s.set_track_preset_dir(dir.clone());
    assert!(s.track_presets().is_empty());
    let vox = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::RenameTrack {
        track: vox,
        name: "Lead Vox".into(),
    })
    .unwrap();
    s.edit(Command::SetTrackVolume {
        track: vox,
        db: -3.0,
    })
    .unwrap();
    s.edit(Command::SetTrackLayout {
        track: vox,
        layout: faderframe_core::ChannelLayout::Stereo,
    })
    .unwrap();
    s.dispatch(Action::SaveTrackPreset { track: vox }).unwrap();
    s.dispatch(Action::SaveTrackPreset { track: vox }).unwrap();
    let names: Vec<_> = s.track_presets().iter().map(|p| p.name.clone()).collect();
    assert_eq!(
        names,
        vec!["Lead Vox", "Lead Vox 2"],
        "saving twice never overwrites"
    );

    let path = s.track_presets()[0].path.clone();
    let before = s.project().tracks.len();
    s.dispatch(Action::AddTrackFromPreset { path: path.clone() })
        .unwrap();
    assert_eq!(s.project().tracks.len(), before + 1);
    let new = *s.selection.tracks.iter().next().unwrap();
    let t = s.project().track(new).unwrap();
    assert_eq!((t.name.as_str(), t.volume_db), ("Lead Vox", -3.0));
    assert_eq!(t.layout, faderframe_core::ChannelLayout::Stereo);

    let other = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::ApplyTrackPreset { track: other, path })
        .unwrap();
    assert_eq!(s.project().track(other).unwrap().volume_db, -3.0);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().track(other).unwrap().volume_db,
        0.0,
        "one undo step"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
