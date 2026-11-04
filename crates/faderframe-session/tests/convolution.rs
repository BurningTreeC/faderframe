#![allow(clippy::unwrap_used)]
//! The Convolution Reverb's impulse response: a picked file is copied into
//! the project like a sampler's sample, loaded as one undo step that also
//! makes the file the space played, named relative to the project file
//! when saved, and played again when the project is opened elsewhere.

use faderframe_core::{ParameterId, PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::convolution::{ImpulseView, id};
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, Session, samples};
use std::path::{Path, PathBuf};

/// A true-stereo response: four channels of decaying noise.
fn impulse(path: &Path) {
    let mut seed = 0x2545_f491u32;
    let channels: Vec<Vec<f32>> = (0..4)
        .map(|_| {
            (0..24_000)
                .map(|n| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    noise * (-(n as f32) / 4_000.0).exp() * 0.5
                })
                .collect()
        })
        .collect();
    faderframe_audio_files::write_wav(
        path,
        &channels,
        44_100,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
}

fn space(s: &mut Session, plugin: PluginInstanceId) -> f64 {
    s.plugin_parameter_value(plugin, ParameterId(id::SPACE))
        .unwrap()
}

fn files(s: &Session, plugin: PluginInstanceId) -> Vec<Option<String>> {
    let (_, slot) = s.plugin_owner(plugin).unwrap();
    samples::doc_of(slot).files
}

/// Tick until the device shows the response named `name` (its designer
/// makes it on a thread of its own).
fn wait_for_view(s: &mut Session, plugin: PluginInstanceId, name: &str) -> ImpulseView {
    for _ in 0..1_000 {
        s.tick(0.01);
        if let Some(v) = s
            .plugin_tap(plugin)
            .and_then(|t| t.assets::<ImpulseView>())
            .filter(|v| v.name == name)
        {
            return (*v).clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the device never showed {name}");
}

fn wait(s: &mut Session, plugin: PluginInstanceId) {
    for _ in 0..500 {
        s.tick(0.01);
        if !s.loading_samples(plugin) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the response never loaded");
}

#[test]
fn an_impulse_file_is_kept_in_the_project_and_played() {
    let outside = std::env::temp_dir().join(format!("ff-ir-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    impulse(&outside.join("church.wav"));

    let mut s = Session::new(Project::new("Space", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(builtin::CONVOLUTION_REVERB, "Convolution Reverb"),
    })
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    // A built-in space at first.
    assert_eq!(space(&mut s, plugin), 5.0);
    wait_for_view(&mut s, plugin, "Hall");

    let steps = s.history_steps().0.len();
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: vec![outside.join("church.wav")],
    })
    .unwrap();
    wait(&mut s, plugin);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Load Impulse Response")
    );
    let media = s.media_dir().to_path_buf();
    let f = files(&s, plugin);
    let copied = PathBuf::from(f[0].as_deref().unwrap());
    assert!(copied.starts_with(media.join(samples::SAMPLES_FOLDER)));
    assert!(copied.exists());
    // The file is what plays now: four channels, its length at the
    // session's rate.
    assert_eq!(space(&mut s, plugin), 0.0);
    let view = wait_for_view(&mut s, plugin, "church");
    assert_eq!(view.channels, 4);
    assert!(
        (view.seconds - 24_000.0 / 44_100.0).abs() < 0.01,
        "{}",
        view.seconds
    );
    assert!(view.problem.is_none());

    // Undo: the hall again, no file.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(space(&mut s, plugin), 5.0);
    assert!(files(&s, plugin).is_empty());
    wait_for_view(&mut s, plugin, "Hall");
    s.dispatch(Action::Redo).unwrap();
    s.wait_for_samples();
    wait_for_view(&mut s, plugin, "church");

    // Saved: the file moves with the media, named relative to the project.
    let dir = std::env::temp_dir().join(format!("ff-ir-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Space.ffproj");
    s.save_as(&path).unwrap();
    let moved_ir = PathBuf::from(files(&s, plugin)[0].clone().unwrap());
    assert!(moved_ir.starts_with(dir.join("Audio").join(samples::SAMPLES_FOLDER)));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(dir.to_string_lossy().as_ref()));

    // Opened elsewhere: the response comes along and plays.
    let elsewhere = std::env::temp_dir().join(format!("ff-ir-moved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::rename(&dir, &elsewhere).unwrap();
    let mut o = Session::new(Project::new("x", 48_000), None, EngineConfig::default()).unwrap();
    o.open(&elsewhere.join("Space.ffproj")).unwrap();
    let plugin = o
        .project()
        .tracks
        .iter()
        .flat_map(|t| t.inserts.iter())
        .find(|p| p.plugin.id == builtin::CONVOLUTION_REVERB)
        .unwrap()
        .id;
    assert!(PathBuf::from(files(&o, plugin)[0].clone().unwrap()).starts_with(&elsewhere));
    o.wait_for_samples();
    let view = wait_for_view(&mut o, plugin, "church");
    assert_eq!(view.channels, 4);
    let _ = std::fs::remove_dir_all(&elsewhere);
    let _ = std::fs::remove_dir_all(&outside);
}

/// Clearing the file leaves the space on the file: the device says there
/// is nothing to play (and plays its input dry).
#[test]
fn a_cleared_file_leaves_nothing_to_play() {
    let outside = std::env::temp_dir().join(format!("ff-ir-clear-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    impulse(&outside.join("box.wav"));
    let mut s = Session::new(Project::new("Space", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(builtin::CONVOLUTION_REVERB, "Convolution Reverb"),
    })
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: vec![outside.join("box.wav")],
    })
    .unwrap();
    wait(&mut s, plugin);
    wait_for_view(&mut s, plugin, "box");
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: vec![],
    })
    .unwrap();
    let view = wait_for_view(&mut s, plugin, "No impulse response");
    assert!(view.problem.is_some());
    assert_eq!(space(&mut s, plugin), 0.0);
    let _ = std::fs::remove_dir_all(&outside);
}
