#![allow(clippy::unwrap_used)]
//! Samples for the built-in samplers: picked files are copied into the
//! project and loaded in the background as one undo step; the project
//! file names them relative to itself, the first save moves them out of
//! the scratch folder, and they come back when the project is opened.

use faderframe_core::{PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::samples::Contents;
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, PluginTarget, Session, samples};
use std::path::{Path, PathBuf};

fn tone(path: &Path, freq: f64) {
    let x: Vec<f32> = (0..12_000)
        .map(|n| (0.5 * (std::f64::consts::TAU * freq * n as f64 / 48_000.0).sin()) as f32)
        .collect();
    faderframe_audio_files::write_wav(
        path,
        &[x],
        48_000,
        faderframe_audio_files::WavFormat::Pcm16,
        false,
    )
    .unwrap();
}

fn drums() -> (Session, PluginInstanceId) {
    let mut s = Session::new(Project::new("Kit", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.place_plugin(
        t,
        PluginTarget::Instrument,
        PluginRef::builtin(builtin::DRUMS, "Drums"),
    )
    .unwrap();
    let plugin = s
        .project()
        .track(t)
        .unwrap()
        .instrument
        .as_ref()
        .unwrap()
        .id;
    (s, plugin)
}

fn wait(s: &mut Session, plugin: PluginInstanceId) {
    for _ in 0..500 {
        s.tick(0.01);
        if !s.loading_samples(plugin) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the samples never loaded");
}

fn files(s: &Session, plugin: PluginInstanceId) -> Vec<Option<String>> {
    let (_, slot) = s.plugin_owner(plugin).unwrap();
    samples::doc_of(slot).files
}

fn loaded(s: &Session, plugin: PluginInstanceId, pad: usize) -> bool {
    s.plugin_tap(plugin)
        .and_then(|t| t.assets::<Contents>())
        .is_some_and(|c| c.set.slot(pad).is_some())
}

#[test]
fn samples_are_copied_in_undone_saved_relative_and_reopened() {
    let outside = std::env::temp_dir().join(format!("ff-samplers-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    tone(&outside.join("kick.wav"), 60.0);
    tone(&outside.join("snare.wav"), 200.0);
    tone(&outside.join("hat.wav"), 5_000.0);

    let (mut s, plugin) = drums();
    // Two loads at once (two pads, then one more): both land.
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: vec![outside.join("kick.wav"), outside.join("snare.wav")],
    })
    .unwrap();
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 5,
        files: vec![outside.join("hat.wav")],
    })
    .unwrap();
    assert!(s.loading_samples(plugin));
    wait(&mut s, plugin);
    let media = s.media_dir().to_path_buf();
    let f = files(&s, plugin);
    assert_eq!(f.len(), 6, "{f:?}");
    for pad in [0, 1, 5] {
        let p = PathBuf::from(f[pad].as_deref().unwrap());
        assert!(
            p.starts_with(media.join(samples::SAMPLES_FOLDER)),
            "copied in: {}",
            p.display()
        );
        assert!(p.exists());
        assert!(loaded(&s, plugin, pad), "pad {pad} plays");
    }
    // Each load is an undo step; clearing a pad is one too.
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 1,
        files: vec![],
    })
    .unwrap();
    assert!(files(&s, plugin)[1].is_none());
    assert!(!loaded(&s, plugin, 1));
    s.dispatch(Action::Undo).unwrap();
    assert!(loaded(&s, plugin, 1), "the snare is back");
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert!(files(&s, plugin).is_empty());
    s.dispatch(Action::Redo).unwrap();
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(files(&s, plugin).len(), 6);

    // The first save moves the samples into the project's media folder
    // and the file names them relative to itself.
    let dir = std::env::temp_dir().join(format!("ff-samplers-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Kit.ffproj");
    s.save_as(&path).unwrap();
    let f = files(&s, plugin);
    let kick = PathBuf::from(f[0].as_deref().unwrap());
    assert!(
        kick.starts_with(dir.join("Audio").join(samples::SAMPLES_FOLDER)),
        "{}",
        kick.display()
    );
    assert!(kick.exists());
    assert!(!media.exists(), "the scratch folder is gone");
    assert!(loaded(&s, plugin, 0), "still plays after the move");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains(dir.to_string_lossy().as_ref()),
        "no absolute paths in the file"
    );

    // Opened elsewhere (the folder moved): the samples come along.
    let moved = std::env::temp_dir().join(format!("ff-samplers-moved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::rename(&dir, &moved).unwrap();
    let mut t = Session::new(Project::new("x", 48_000), None, EngineConfig::default()).unwrap();
    t.open(&moved.join("Kit.ffproj")).unwrap();
    let plugin = t
        .project()
        .tracks
        .iter()
        .find_map(|tr| tr.instrument.as_ref().map(|i| i.id))
        .unwrap();
    let f = files(&t, plugin);
    assert!(
        PathBuf::from(f[5].as_deref().unwrap()).starts_with(&moved),
        "{f:?}"
    );
    assert!(loaded(&t, plugin, 5));
    let _ = std::fs::remove_dir_all(&moved);
    let _ = std::fs::remove_dir_all(&outside);
}
