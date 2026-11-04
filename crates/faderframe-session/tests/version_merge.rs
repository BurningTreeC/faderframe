//! Merging versions: versions of the project's own, and a copy someone
//! else worked on in another folder, joined with the project as it is.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::merge::{ConflictKind, Resolution};
use faderframe_project::{Command, Project, SourceSpec, file};
use faderframe_session::compare::Comparing;
use faderframe_session::{Action, MEDIA_FOLDER, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-merge-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
        .id
}

fn edit(s: &mut Session, cmd: Command) {
    s.dispatch(Action::Edit(cmd)).unwrap();
}

fn version(s: &mut Session, name: &str) -> PathBuf {
    s.dispatch(Action::SaveVersion { name: name.into() })
        .unwrap();
    s.versions().last().unwrap().path.clone()
}

#[test]
fn a_version_set_aside_merges_back_as_one_undo_step() {
    let dir = tmp("aside");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.save_as(&dir.join("song.ffproj")).unwrap();
    let saved = file::read_lineage(&dir.join("song.ffproj")).unwrap();
    assert_eq!(s.project_lineage(), Some(&saved));
    let start = version(&mut s, "Start");
    let bass = track(&s, "Bass");
    let drums = track(&s, "Drums");
    // One line: the bass down. Set aside by restoring the start.
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: bass,
            db: -12.0,
        },
    );
    s.dispatch(Action::RestoreVersion(start.clone())).unwrap();
    let aside = s.versions().last().unwrap().clone();
    assert_eq!(aside.name, "Before restoring 1");
    // The other: the drums renamed.
    edit(
        &mut s,
        Command::RenameTrack {
            track: drums,
            name: "Kit".into(),
        },
    );
    let steps = s.history_steps().0.len();
    s.dispatch(Action::MergeVersion {
        theirs: aside.path.clone(),
        base: None,
    })
    .unwrap();
    let status = s.merge_status().unwrap();
    assert!(status.conflicts.is_empty(), "{:#?}", status.conflicts);
    assert_eq!(status.label, "Merge Version 2 ‘Before restoring 1’");
    assert_eq!(status.base, "version 1 ‘Start’");
    assert!(
        status.changes.iter().any(|c| c.contains("volume")),
        "{:#?}",
        status.changes
    );
    assert_eq!(s.project().track(bass).unwrap().volume_db, -12.0);
    assert_eq!(s.project().track(drums).unwrap().name, "Kit");
    assert!(s.is_dirty());
    // One step: undone, the merge is gone; redone, back.
    let (done, _) = s.history_steps();
    assert_eq!(done.len(), steps + 1);
    assert_eq!(done.last().unwrap(), "Merge Version 2 ‘Before restoring 1’");
    let aside_id = file::read_lineage(&aside.path).unwrap().id;
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -3.0);
    assert_eq!(s.project().track(drums).unwrap().name, "Kit");
    // Undone, the project no longer descends from the version.
    assert!(!s.project_lineage().unwrap().knows(&aside_id));
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -12.0);
    // The merged state descends from both: merging the version again has
    // nothing new.
    assert!(
        s.project_lineage()
            .unwrap()
            .knows(&file::read_lineage(&aside.path).unwrap().id)
    );
    assert!(
        s.dispatch(Action::MergeVersion {
            theirs: aside.path,
            base: None
        })
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conflicts_are_listed_and_resolved_in_the_merge_step() {
    let dir = tmp("conflicts");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.save_as(&dir.join("song.ffproj")).unwrap();
    let start = version(&mut s, "Start");
    let bass = track(&s, "Bass");
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: bass,
            db: -12.0,
        },
    );
    s.dispatch(Action::RestoreVersion(start.clone())).unwrap();
    let aside = s.versions().last().unwrap().path.clone();
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: bass,
            db: -6.0,
        },
    );
    let steps = s.history_steps().0.len();
    s.take_ui_requests();
    // The base given by hand this time.
    s.dispatch(Action::MergeVersion {
        theirs: aside.clone(),
        base: Some(start.clone()),
    })
    .unwrap();
    assert!(s.take_ui_requests().contains(&UiRequest::MergeConflicts));
    let status = s.merge_status().unwrap();
    assert_eq!(status.conflicts.len(), 1, "{:#?}", status.conflicts);
    let c = &status.conflicts[0];
    assert_eq!(c.kind, ConflictKind::BothChanged);
    assert_eq!(c.what, "Track ‘Bass’");
    assert_eq!(c.chosen, Resolution::Ours);
    assert!(status.open);
    assert_eq!(s.project().track(bass).unwrap().volume_db, -6.0);
    // Theirs: the step changes in place.
    s.dispatch(Action::ResolveMergeConflict {
        key: c.key.clone(),
        resolution: Resolution::Theirs,
    })
    .unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -12.0);
    assert_eq!(
        s.merge_status().unwrap().conflicts[0].chosen,
        Resolution::Theirs
    );
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // "Both" is no choice for a track.
    assert!(
        s.dispatch(Action::ResolveMergeConflict {
            key: c.key.clone(),
            resolution: Resolution::Both,
        })
        .is_err()
    );
    // Hearing the other choice: rendered beside the merge as it is.
    s.dispatch(Action::AuditionMergeConflict { key: c.key.clone() })
        .unwrap();
    assert!(s.wait_for_compare(Duration::from_secs(120)));
    let status = s.compare_status().unwrap();
    assert_eq!(status.what, Comparing::MergeChoice);
    assert!(status.ready, "{status:?}");
    s.dispatch(Action::Compare(faderframe_session::compare::CompareOp::Off))
        .unwrap();
    // One undo takes the whole merge back.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -6.0);
    // Once something else is done, the merge's choices are closed.
    assert!(!s.merge_status().unwrap().open);
    assert!(
        s.dispatch(Action::ResolveMergeConflict {
            key: c.key.clone(),
            resolution: Resolution::Ours,
        })
        .is_err()
    );
    s.dispatch(Action::CloseMerge).unwrap();
    assert!(s.merge_status().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

fn tone(path: &Path, hz: f32) {
    let ch: Vec<f32> = (0..9_600)
        .map(|i| (i as f32 * hz / 48_000.0 * std::f32::consts::TAU).sin() * 0.5)
        .collect();
    write_wav(path, &[ch], 48_000, WavFormat::Pcm24, false).unwrap();
}

fn import(s: &mut Session, file: &Path) {
    s.dispatch(Action::ImportFiles {
        files: vec![file.to_path_buf()],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
}

fn file_sources(p: &Project) -> Vec<PathBuf> {
    p.sources
        .values()
        .filter_map(|s| match &s.spec {
            SourceSpec::File { path, .. } => Some(path.clone()),
            SourceSpec::Generated { .. } => None,
        })
        .collect()
}

#[test]
fn a_copy_from_another_folder_merges_and_its_media_are_found_by_content() {
    let root = tmp("copy");
    let raw = root.join("raw");
    std::fs::create_dir_all(&raw).unwrap();
    tone(&raw.join("Vox.wav"), 220.0);
    tone(&raw.join("Gtr.wav"), 330.0);
    tone(&raw.join("Keys.wav"), 440.0);
    // Ours: a project with a vocal, saved.
    let mut s = Session::new(Project::new("Song", 48_000), None, EngineConfig::default()).unwrap();
    import(&mut s, &raw.join("Vox.wav"));
    let ours_file = root.join("Mine").join("Song.ffproj");
    s.save_as(&ours_file).unwrap();
    // Their copy: the folder copied elsewhere, opened, a guitar and keys
    // added, the vocal renamed, saved.
    let theirs_dir = root.join("Theirs");
    std::fs::create_dir_all(theirs_dir.join(MEDIA_FOLDER)).unwrap();
    std::fs::copy(&ours_file, theirs_dir.join("Song.ffproj")).unwrap();
    for f in std::fs::read_dir(ours_file.parent().unwrap().join(MEDIA_FOLDER)).unwrap() {
        let f = f.unwrap().path();
        std::fs::copy(
            &f,
            theirs_dir.join(MEDIA_FOLDER).join(f.file_name().unwrap()),
        )
        .unwrap();
    }
    let mut t = Session::new(Project::new("x", 48_000), None, EngineConfig::default()).unwrap();
    t.open(&theirs_dir.join("Song.ffproj")).unwrap();
    import(&mut t, &raw.join("Gtr.wav"));
    import(&mut t, &raw.join("Keys.wav"));
    let vox = t
        .project()
        .tracks
        .iter()
        .find(|x| x.name.contains("Vox"))
        .unwrap()
        .id;
    t.dispatch(Action::Edit(Command::RenameTrack {
        track: vox,
        name: "Lead Vocal".into(),
    }))
    .unwrap();
    t.save_as(&theirs_dir.join("Song.ffproj")).unwrap();
    // They send the project file and the guitar only: their vocal and keys
    // files are not here. Our vocal is the same take (found by content);
    // their keys are nowhere.
    let theirs_media = theirs_dir.join(MEDIA_FOLDER);
    let gtr = std::fs::read_dir(&theirs_media)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name().unwrap().to_string_lossy().starts_with("Gtr")
                && p.extension().is_some_and(|e| e == "wav")
        })
        .unwrap();
    for f in std::fs::read_dir(&theirs_media).unwrap() {
        let f = f.unwrap().path();
        if f != gtr && f.extension().is_some_and(|e| e == "wav") {
            std::fs::remove_file(f).unwrap();
        }
    }
    // Ours meanwhile: the vocal louder.
    let our_vox = s
        .project()
        .tracks
        .iter()
        .find(|x| x.name.contains("Vox"))
        .unwrap()
        .id;
    edit(
        &mut s,
        Command::SetTrackVolume {
            track: our_vox,
            db: -2.0,
        },
    );
    s.dispatch(Action::MergeProjectFile(theirs_dir.join("Song.ffproj")))
        .unwrap();
    let status = s.merge_status().unwrap();
    assert!(status.conflicts.is_empty(), "{:#?}", status.conflicts);
    assert_eq!(status.base, "the state both come from");
    let names: Vec<&str> = s.project().tracks.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"Lead Vocal"), "{names:?}");
    assert!(names.iter().any(|n| n.contains("Gtr")) && names.iter().any(|n| n.contains("Keys")));
    let t_vox = s.project().track(our_vox).unwrap();
    assert_eq!((t_vox.name.as_str(), t_vox.volume_db), ("Lead Vocal", -2.0));
    // The vocal is still our file; the guitar is used where it is; the
    // keys are missing.
    let ours_media = ours_file.parent().unwrap().join(MEDIA_FOLDER);
    let files = file_sources(s.project());
    assert_eq!(files.len(), 3, "{files:?}");
    assert!(files.iter().any(|f| f.starts_with(&ours_media)));
    assert!(
        files.contains(&gtr),
        "{gtr:?} in {files:?} ({:?})",
        status.media
    );
    assert_eq!(status.media.elsewhere, [gtr.display().to_string()]);
    assert_eq!(status.media.missing.len(), 1, "{:?}", status.media);
    assert!(status.media.missing[0].starts_with("Keys"));
    // Their next file merges from the state merged in: nothing twice.
    s.save_as(&ours_file).unwrap();
    let keys = t
        .project()
        .tracks
        .iter()
        .find(|x| x.name.contains("Keys"))
        .unwrap()
        .id;
    t.dispatch(Action::Edit(Command::SetTrackVolume {
        track: keys,
        db: -9.0,
    }))
    .unwrap();
    t.save_as(&theirs_dir.join("Song.ffproj")).unwrap();
    s.dispatch(Action::MergeProjectFile(theirs_dir.join("Song.ffproj")))
        .unwrap();
    let status = s.merge_status().unwrap();
    assert!(status.conflicts.is_empty(), "{:#?}", status.conflicts);
    let keys_tracks: Vec<_> = s
        .project()
        .tracks
        .iter()
        .filter(|x| x.name.contains("Keys"))
        .collect();
    assert_eq!(keys_tracks.len(), 1, "their keys came in once");
    assert_eq!(keys_tracks[0].volume_db, -9.0);
    assert_eq!(
        s.project()
            .tracks
            .iter()
            .filter(|x| x.name.contains("Gtr"))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn saves_stamp_where_they_come_from_and_keep_bases() {
    let dir = tmp("stamps");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    assert!(s.project_lineage().is_none());
    let path = dir.join("song.ffproj");
    s.save_as(&path).unwrap();
    let first = file::read_lineage(&path).unwrap();
    assert!(first.ancestors.is_empty());
    let v = version(&mut s, "One");
    let one = file::read_lineage(&v).unwrap();
    assert_eq!(one.ancestors[0].id, first.id);
    s.save_as(&path).unwrap();
    let again = file::read_lineage(&path).unwrap();
    assert_eq!(again.ancestors[0].id, one.id);
    assert_eq!(again.generation, 2);
    // The saved states are kept as bases (the newest only).
    let bases = dir.join("Versions").join(".bases");
    for _ in 0..10 {
        s.save_as(&path).unwrap();
    }
    let kept = std::fs::read_dir(&bases).unwrap().count();
    assert_eq!(kept, faderframe_session::version_merge::KEPT_BASES);
    // Reopened: the project goes on from the file's stamp.
    let mut s2 = Session::new(Project::new("x", 48_000), None, EngineConfig::default()).unwrap();
    s2.open(&path).unwrap();
    assert_eq!(
        s2.project_lineage(),
        Some(&file::read_lineage(&path).unwrap())
    );
    // The versions list is untouched by the bases.
    assert_eq!(s2.versions().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
