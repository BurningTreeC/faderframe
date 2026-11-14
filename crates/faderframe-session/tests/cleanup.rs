//! Unused media at close: files of the project's Audio folder that the
//! project brought there and neither the saved project nor its versions
//! use — never another project's (projects in one folder share it).
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, SourceSpec};
use faderframe_session::{Action, MEDIA_FOLDER, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-cleanup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(path: &Path) {
    let ch: Vec<f32> = (0..4_800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    write_wav(path, &[ch], 48_000, WavFormat::Pcm24, false).unwrap();
}

/// Import `file` and return its clip and the file it plays.
fn import(s: &mut Session, file: &Path) -> (faderframe_core::ClipId, PathBuf) {
    let before: Vec<_> = s.project().clips.keys().copied().collect();
    s.dispatch(Action::ImportFiles {
        files: vec![file.to_path_buf()],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let (id, clip) = s
        .project()
        .clips
        .iter()
        .find(|(id, _)| !before.contains(id))
        .unwrap();
    let source = clip.as_audio().unwrap().source;
    let SourceSpec::File { path, .. } = &s.project().sources[&source].spec else {
        panic!()
    };
    (*id, path.clone())
}

fn names(files: &[PathBuf]) -> Vec<String> {
    let mut v: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn only_files_no_saved_state_uses_are_unused() {
    let dir = tmp("project");
    for n in ["Kick", "Bass", "Pad"] {
        tone(&dir.join(format!("{n}.wav")));
    }
    let mut s = Session::new(Project::new("Clean", 48_000), None, EngineConfig::default()).unwrap();
    assert!(s.unused_media().is_none(), "never saved: nothing to say");
    import(&mut s, &dir.join("Kick.wav"));
    let (bass_clip, _) = import(&mut s, &dir.join("Bass.wav"));
    let project = dir.join("Song").join("Song.ffproj");
    s.save_as(&project).unwrap();
    let media = project.parent().unwrap().join(MEDIA_FOLDER);
    assert_eq!(s.unused_media().unwrap().files, Vec::<PathBuf>::new());
    // A version keeps the bass; then its clip goes.
    s.dispatch(Action::SaveVersion {
        name: "with bass".into(),
    })
    .unwrap();
    s.dispatch(Action::Edit(Command::RemoveClip { clip: bass_clip }))
        .unwrap();
    // The pad comes and goes after the version.
    let (pad_clip, pad) = import(&mut s, &dir.join("Pad.wav"));
    s.dispatch(Action::Edit(Command::RemoveClip { clip: pad_clip }))
        .unwrap();
    s.save().unwrap();
    // A file no source of the project names (another project's, one put
    // there by hand), and a sample: both left alone.
    tone(&media.join("Stray.wav"));
    std::fs::create_dir_all(media.join("Samples")).unwrap();
    tone(&media.join("Samples").join("Hit.wav"));

    let unused = s.unused_media().unwrap();
    let got = names(&unused.files);
    let pad_name = pad.file_name().unwrap().to_string_lossy().to_string();
    assert!(got.contains(&pad_name), "{got:?}");
    assert!(!got.contains(&"Stray.wav".to_string()), "{got:?}");
    assert!(
        got.iter()
            .all(|n| n.starts_with(pad.file_stem().unwrap().to_str().unwrap())),
        "only the pad and its peaks: {got:?}"
    );
    assert!(
        !got.iter().any(|n| n.starts_with("Bass")),
        "a version uses it"
    );
    assert!(!got.iter().any(|n| n.starts_with("Kick")));
    assert!(unused.bytes > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two projects saved in one folder share its Audio folder: closing one
/// never offers the other's recordings, only what it brought there itself
/// and no longer uses.
#[test]
fn another_projects_files_in_the_same_folder_are_never_offered() {
    let dir = tmp("shared");
    for n in ["Vocal", "Master", "Scrap"] {
        tone(&dir.join(format!("{n}.wav")));
    }
    let folder = dir.join("Songs");
    // The song: a vocal, kept.
    let mut song =
        Session::new(Project::new("Song", 48_000), None, EngineConfig::default()).unwrap();
    import(&mut song, &dir.join("Vocal.wav"));
    song.save_as(&folder.join("Song.ffproj")).unwrap();
    // The master, beside it: its mix, and a scrap it no longer uses.
    let mut master = Session::new(
        Project::new("Master", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    import(&mut master, &dir.join("Master.wav"));
    let (scrap_clip, scrap) = import(&mut master, &dir.join("Scrap.wav"));
    master
        .dispatch(Action::Edit(Command::RemoveClip { clip: scrap_clip }))
        .unwrap();
    master.save_as(&folder.join("Master.ffproj")).unwrap();
    let media = folder.join(MEDIA_FOLDER);
    assert!(media.join("Vocal.wav").is_file() && media.join("Master.wav").is_file());

    let got = names(&master.unused_media().unwrap().files);
    let scrap_stem = scrap.file_stem().unwrap().to_string_lossy().to_string();
    assert!(got.iter().any(|n| n.starts_with(&scrap_stem)), "{got:?}");
    assert!(
        got.iter().all(|n| n.starts_with(&scrap_stem)),
        "only the master's own scrap, never the song's vocal: {got:?}"
    );
    // And the other way round: the song offers nothing of the master's.
    assert_eq!(song.unused_media().unwrap().files, Vec::<PathBuf>::new());
    let _ = std::fs::remove_dir_all(&dir);
}
