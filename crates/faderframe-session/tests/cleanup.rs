//! Unused media at close: files of the project's Audio folder that neither
//! the saved project nor its versions use.
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
    // A stray file, and a sample (left alone).
    tone(&media.join("Stray.wav"));
    std::fs::create_dir_all(media.join("Samples")).unwrap();
    tone(&media.join("Samples").join("Hit.wav"));

    let unused = s.unused_media().unwrap();
    let got = names(&unused.files);
    let pad_name = pad.file_name().unwrap().to_string_lossy().to_string();
    assert!(got.contains(&pad_name), "{got:?}");
    assert!(got.contains(&"Stray.wav".to_string()), "{got:?}");
    assert!(
        got.iter().all(|n| n == &pad_name
            || n == "Stray.wav"
            || n.starts_with(pad.file_stem().unwrap().to_str().unwrap())),
        "only the pad (and its peaks) and the stray: {got:?}"
    );
    assert!(
        !got.iter().any(|n| n.starts_with("Bass")),
        "a version uses it"
    );
    assert!(!got.iter().any(|n| n.starts_with("Kick")));
    assert!(unused.bytes > 0);
    let _ = std::fs::remove_dir_all(&dir);
}
