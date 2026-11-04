//! Opt-in: an installed ARA plug-in (VST3 or CLAP) in a live session — the
//! ARA SDK's ARATestPlugIn, Melodyne, SpectraLayers …: discovered as ARA,
//! a clip opened with it plays through it, its document is archived on
//! saving and restored on opening, and it reports nothing amiss.
//!
//! `FADERFRAME_TEST_ARA=/path/ARATestPlugIn.vst3 cargo test -p
//! faderframe-session --test ara_installed -- --ignored --nocapture`
//! (several bundles separated by `:`).
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginFormat, PluginRef, Project};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn describe(
    bundle: &std::path::Path,
) -> (
    PluginFormat,
    Vec<faderframe_plugin_host::scan::ScannedPlugin>,
) {
    match bundle.extension().and_then(|e| e.to_str()) {
        Some("vst3") => {
            let p = faderframe_plugin_vst3::scan::describe_bundle(bundle).unwrap();
            faderframe_plugin_vst3::set_catalog(p.clone());
            (PluginFormat::Vst3, p)
        }
        _ => {
            let p = faderframe_plugin_clap::scan::describe_bundle(bundle).unwrap();
            faderframe_plugin_clap::set_catalog(p.clone());
            (PluginFormat::Clap, p)
        }
    }
}

fn run(s: &mut Session, seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs FADERFRAME_TEST_ARA with an installed ARA plug-in bundle"]
fn an_installed_ara_plugin_renders_and_keeps_its_document() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
        r.add_factory(Box::new(faderframe_plugin_vst3::Vst3Factory::new()));
        r
    });
    let bundles: Vec<PathBuf> = std::env::var("FADERFRAME_TEST_ARA")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let dir = std::env::temp_dir().join(format!("ff-ara-installed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("tone.wav");
    let tone: Vec<f32> = (0..2 * SR as usize)
        .map(|i| 0.4 * (std::f64::consts::TAU * 220.0 * i as f64 / f64::from(SR)).sin() as f32)
        .collect();
    write_wav(&file, &[tone], SR, WavFormat::Float32, false).unwrap();
    let asserts = faderframe_plugin_host::ara::assert_count();
    for bundle in bundles {
        let (format, plugins) = describe(&bundle);
        let ara: Vec<_> = plugins
            .iter()
            .filter(|p| p.features.iter().any(|f| f.starts_with("ara:")))
            .collect();
        assert!(
            !ara.is_empty(),
            "{}: no ARA plug-in found",
            bundle.display()
        );
        for plugin in ara {
            let mut s =
                Session::new(Project::new("ARA", SR), None, EngineConfig::default()).unwrap();
            s.start_audio(
                vec![Box::new(DummyBackend::default())],
                &AudioPreferences::default(),
            )
            .unwrap();
            s.dispatch(Action::ImportFiles {
                files: vec![file.clone()],
                track: None,
                at: MusicalTime::ZERO,
            })
            .unwrap();
            s.wait_for_imports();
            let clip = *s.project().clips.keys().next().unwrap();
            let track = s.project().clip(clip).unwrap().track;
            let reference = PluginRef {
                format,
                id: plugin.id.clone(),
                name: plugin.name.clone(),
            };
            assert!(s.ara_plugins().contains(&reference), "listed as ARA");
            s.dispatch(Action::OpenWithAra {
                clip,
                plugin: Some(reference),
            })
            .unwrap();
            assert_eq!(s.ara_rendered_tracks(), vec![track], "bound");
            // Let it analyse, then play the clip through it.
            run(&mut s, 2.0);
            s.dispatch(Action::Transport(TransportAction::Play))
                .unwrap();
            run(&mut s, 1.5);
            let level = s.meter(track).left.level_db;
            eprintln!(
                "{format:?} {}: the clip plays at {level:.1} dBFS",
                plugin.name
            );
            assert!(level > -40.0, "{}: silent ({level:.1} dBFS)", plugin.name);
            s.dispatch(Action::Transport(TransportAction::Stop))
                .unwrap();
            let path = dir.join(format!("{}.ffproj", plugin.id.replace(['/', ':'], "_")));
            s.save_as(&path).unwrap();
            assert_eq!(s.project().ara.archives.len(), 1, "archived");
            drop(s);
            let mut s =
                Session::new(Project::new("other", SR), None, EngineConfig::default()).unwrap();
            s.open(&path).unwrap();
            assert_eq!(s.ara_rendered_tracks().len(), 1, "bound again");
            for (p, file, done) in s.ara_analysis() {
                eprintln!("  {p} analysing {file}: {:.0} %", done * 100.0);
            }
        }
    }
    assert_eq!(
        faderframe_plugin_host::ara::assert_count(),
        asserts,
        "ARA asserts: the host broke a rule"
    );
}
