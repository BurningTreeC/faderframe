//! Opt-in: installed plugins in a live session (dummy device, playing).
//!
//! `FADERFRAME_TEST_BUNDLES=~/.clap/Vendor/Fx.clap cargo test -p
//! faderframe-session --test installed_plugins -- --ignored --nocapture`
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginFormat, PluginRef};
use faderframe_session::{Action, AudioPreferences, PluginTarget, Session, TransportAction};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A bundle's plugins: CLAP, VST3 or (Linux) LV2, made the catalog.
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
        #[cfg(target_os = "linux")]
        Some("lv2") => {
            let p = faderframe_plugin_lv2::scan::describe_bundle(bundle)
                .unwrap()
                .plugins;
            let scanned = p.iter().map(|x| x.scanned()).collect();
            faderframe_plugin_lv2::set_catalog(p);
            (PluginFormat::Lv2, scanned)
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
#[ignore = "needs FADERFRAME_TEST_BUNDLES with installed CLAP/VST3 effect bundles"]
fn inserting_an_effect_while_playing_keeps_the_track_audible() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
        r.add_factory(Box::new(faderframe_plugin_vst3::Vst3Factory::new()));
        #[cfg(target_os = "linux")]
        r.add_factory(Box::new(faderframe_plugin_lv2::Lv2Factory::new()));
        r
    });
    let bundles: Vec<PathBuf> = std::env::var("FADERFRAME_TEST_BUNDLES")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let mut failures = Vec::new();
    for bundle in bundles {
        let (format, plugins) = describe(&bundle);
        for plugin in plugins.iter().filter(|p| !p.is_instrument()) {
            let mut s = Session::demo(EngineConfig::default()).unwrap();
            s.start_audio(
                vec![Box::new(DummyBackend::default())],
                &AudioPreferences::default(),
            )
            .unwrap();
            let pluck = s
                .project()
                .tracks
                .iter()
                .find(|t| t.name == "Pluck")
                .unwrap()
                .id;
            s.dispatch(Action::Transport(TransportAction::Play))
                .unwrap();
            run(&mut s, 0.6);
            let before = s.meter(pluck).left.level_db;
            s.place_plugin(
                pluck,
                PluginTarget::Insert(0),
                PluginRef {
                    format,
                    id: plugin.id.clone(),
                    name: plugin.name.clone(),
                },
            )
            .unwrap();
            run(&mut s, 1.0);
            let after = s.meter(pluck).left.level_db;
            eprintln!(
                "{format:?} {}: Pluck {before:.1} dBFS → {after:.1} dBFS",
                plugin.name
            );
            if after < before - 30.0 {
                failures.push(format!(
                    "{format:?} {}: {before:.1} → {after:.1} dBFS",
                    plugin.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "needs FADERFRAME_TEST_BUNDLES with an installed instrument bundle"]
fn instruments_play_live_midi_as_instrument_or_insert() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
        r.add_factory(Box::new(faderframe_plugin_vst3::Vst3Factory::new()));
        #[cfg(target_os = "linux")]
        r.add_factory(Box::new(faderframe_plugin_lv2::Lv2Factory::new()));
        r
    });
    let bundles: Vec<PathBuf> = std::env::var("FADERFRAME_TEST_BUNDLES")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let mut failures = Vec::new();
    for bundle in bundles {
        let (format, plugins) = describe(&bundle);
        for plugin in plugins.iter().filter(|p| p.is_instrument()) {
            let pref = PluginRef {
                format,
                id: plugin.id.clone(),
                name: plugin.name.clone(),
            };
            let mut s = Session::demo(EngineConfig::default()).unwrap();
            s.start_audio(
                vec![Box::new(DummyBackend::default())],
                &AudioPreferences::default(),
            )
            .unwrap();
            // A new instrument track has no instrument; the synth chosen for
            // its first insert slot remains visible in the insert chain.
            let t = s
                .add_track(faderframe_project::TrackKind::Instrument)
                .unwrap();
            assert!(s.project().track(t).unwrap().instrument.is_none());
            s.place_plugin(t, PluginTarget::Insert(0), pref.clone())
                .unwrap();
            let track = s.project().track(t).unwrap();
            assert!(track.instrument.is_none() && track.inserts.len() == 1);
            // The selected instrument track plays the keyboard.
            run(&mut s, 0.3);
            s.midi_keyboard().send(&[0x90, 60, 110]);
            run(&mut s, 0.8);
            let level = s.meter(t).left.level_db;
            eprintln!("{format:?} {} as instrument: {level:.1} dBFS", plugin.name);
            if level < -60.0 {
                failures.push(format!("{format:?} {}: silent as instrument", plugin.name));
            }
            s.midi_keyboard().send(&[0x80, 60, 0]);
            // As an insert after another instrument it gets the MIDI too.
            let lead = s
                .project()
                .tracks
                .iter()
                .find(|t| t.name == "Lead Synth")
                .unwrap()
                .id;
            s.dispatch(Action::SelectTracks {
                tracks: vec![lead],
                mode: faderframe_session::SelectMode::Replace,
            })
            .unwrap();
            let before = s.project().track(lead).unwrap().inserts.len();
            s.place_plugin(lead, PluginTarget::Insert(before), pref.clone())
                .unwrap();
            assert_eq!(s.project().track(lead).unwrap().inserts.len(), before + 1);
            run(&mut s, 0.3);
            s.midi_keyboard().send(&[0x90, 64, 110]);
            run(&mut s, 0.8);
            let level = s.meter(lead).left.level_db;
            eprintln!("{format:?} {} as an insert: {level:.1} dBFS", plugin.name);
            if level < -60.0 {
                failures.push(format!("{format:?} {}: silent as an insert", plugin.name));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
