//! Opt-in: render installed plugins through the whole engine.
//!
//! `FADERFRAME_TEST_BUNDLES=~/.clap/Vendor/Fx.clap:~/.vst3/Vendor/Synth.vst3
//! cargo test -p faderframe-bench --test installed_plugins -- --ignored --nocapture`
//!
//! Effects go on a mono and a stereo audio track playing a tone (the track
//! must not go silent); instruments go on an instrument track playing a MIDI
//! clip (it must sound).
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{AudioData, GeneratorSpec};
use faderframe_core::{AudioSourceId, ChannelLayout, TrackId};
use faderframe_engine::offline::render_project;
use faderframe_engine::{EngineConfig, SourceMap};
use faderframe_plugin_host::scan::ScannedPlugin;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, MidiClip, MidiNote, PluginFormat,
    PluginRef, PluginSlot, Project, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::sync::Arc;

const SR: u32 = 48_000;

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

fn registry() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
        r.add_factory(Box::new(faderframe_plugin_vst3::Vst3Factory::new()));
        #[cfg(target_os = "linux")]
        r.add_factory(Box::new(faderframe_plugin_lv2::Lv2Factory::new()));
        r
    });
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn slot(p: &mut Project, format: PluginFormat, plugin: &ScannedPlugin) -> PluginSlot {
    PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef {
            format,
            id: plugin.id.clone(),
            name: plugin.name.clone(),
        },
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    }
}

/// An audio track (before the master) playing a 220 Hz tone.
fn tone_track(p: &mut Project, sources: &mut SourceMap, layout: ChannelLayout) -> TrackId {
    let id: TrackId = p.ids.allocate();
    let t = Track::new(id, TrackKind::Audio, "tone", TrackColor::palette(0)).with_layout(layout);
    let at = p.tracks.len() - 1;
    p.tracks.insert(at, t);
    let frames = SR as usize;
    let tone: Vec<f32> = (0..frames)
        .map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / SR as f32).sin() * 0.5)
        .collect();
    let src: AudioSourceId = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: "tone".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Silence {
                    seconds: 1.0,
                    channels: layout.channel_count() as u16,
                },
            },
        },
    );
    sources.insert(
        src,
        Arc::new(AudioData::from_channels(
            SR,
            vec![tone; layout.channel_count()],
        ))
        .into(),
    );
    let clip = p.ids.allocate();
    p.clips.insert(
        clip,
        Clip {
            id: clip,
            track: id,
            name: "tone".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: src,
                source_offset: 0,
                length: frames as i64,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
            }),
        },
    );
    p.track_mut(id).unwrap().clips.push(clip);
    id
}

fn render(p: &Project, sources: &SourceMap) -> f32 {
    let out = render_project(p, sources, EngineConfig::default(), 256, 0, SR as usize / 2).unwrap();
    rms(&out[0][SR as usize / 10..])
}

fn bundles() -> Vec<PathBuf> {
    std::env::var("FADERFRAME_TEST_BUNDLES")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[test]
#[ignore = "needs FADERFRAME_TEST_BUNDLES with installed CLAP/VST3 bundles"]
fn installed_plugins_pass_audio_and_play_notes() {
    registry();
    let mut failures = Vec::new();
    for bundle in bundles() {
        let (format, plugins) = describe(&bundle);
        for plugin in &plugins {
            let label = format!(
                "{:?} {} ({:?} in, {:?} out)",
                format, plugin.name, plugin.audio_inputs, plugin.audio_outputs
            );
            if plugin.is_instrument() {
                let mut p = Project::new("inst", SR);
                let id: TrackId = p.ids.allocate();
                let mut t = Track::new(id, TrackKind::Instrument, "inst", TrackColor::palette(1))
                    .with_layout(ChannelLayout::Stereo);
                t.instrument = Some(slot(&mut p, format, plugin));
                let at = p.tracks.len() - 1;
                p.tracks.insert(at, t);
                let clip = p.ids.allocate();
                let notes = [48u8, 55, 60, 64]
                    .into_iter()
                    .map(|key| MidiNote {
                        id: p.ids.allocate(),
                        start: MusicalTime::ZERO,
                        length: MusicalTime::from_quarters(3.0),
                        key,
                        velocity: 110,
                        channel: 0,
                        muted: false,
                    })
                    .collect();
                p.clips.insert(
                    clip,
                    Clip {
                        id: clip,
                        track: id,
                        name: "notes".into(),
                        color: None,
                        start: MusicalTime::ZERO,
                        muted: false,
                        content: ClipContent::Midi(MidiClip {
                            length: MusicalTime::from_quarters(4.0),
                            notes,
                            controllers: Vec::new(),
                            expressions: Vec::new(),
                            sysex: Vec::new(),
                        }),
                    },
                );
                p.track_mut(id).unwrap().clips.push(clip);
                let level = render(&p, &SourceMap::new());
                eprintln!("{label}: instrument level {level:.4}");
                if level < 1e-3 {
                    failures.push(format!("{label}: silent instrument"));
                }
            } else {
                for layout in [ChannelLayout::Mono, ChannelLayout::Stereo] {
                    let mut p = Project::new("fx", SR);
                    let mut sources = SourceMap::new();
                    let t = tone_track(&mut p, &mut sources, layout);
                    let dry = render(&p, &sources);
                    let s = slot(&mut p, format, plugin);
                    p.track_mut(t).unwrap().inserts.push(s);
                    let wet = render(&p, &sources);
                    eprintln!("{label} on a {layout:?} track: dry {dry:.4}, through it {wet:.4}");
                    if wet < dry * 0.1 {
                        failures.push(format!(
                            "{label}: {layout:?} track goes silent ({dry:.4} → {wet:.4})"
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
