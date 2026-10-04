//! An album song's inserts: hosted while the song exists, and — for the
//! monitored song — run after the master strip.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ParameterId, PluginInstanceId, SongId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::album::{Song, SongSource};
use faderframe_project::demo::demo_project;
use faderframe_project::{Impact, PluginRef, PluginSlot, Project, SavedParameter};

const SR: u32 = 48_000;

fn gain(id: u64, db: f64) -> PluginSlot {
    PluginSlot {
        id: PluginInstanceId(id),
        plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(0),
            value: db,
        }],
        state: None,
        sidechain: None,
    }
}

fn project() -> Project {
    let mut p = demo_project(SR);
    for (i, db) in [(0u64, -6.0), (1, -60.0)] {
        let mut s = Song::new(
            SongId(70_000 + i),
            format!("Song {i}"),
            SongSource::ThisProject,
        );
        s.inserts.push(gain(71_000 + i, db));
        p.album.songs.push(s);
    }
    p
}

/// Peak of the master output over two seconds of the demo.
fn peak(p: &Project, monitor: Option<SongId>) -> (f32, OfflineRenderer) {
    let sources = render_generated_sources(p, SR);
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: 256,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(p, &sources, config, 256, 2).unwrap();
    r.controller.set_album_monitor(monitor);
    r.controller.sync(p, &sources, Impact::Graph).unwrap();
    r.play_from(SR as i64 * 2).unwrap();
    let out = r.render(SR as usize * 2);
    let peak = out[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (peak, r)
}

#[test]
fn the_monitored_songs_inserts_run_after_the_master() {
    let p = project();
    let (plain, r) = peak(&p, None);
    assert!(plain > 0.01, "the demo sounds: {plain}");
    // Not monitored, still hosted: its parameters can be shown and edited.
    assert!(
        r.controller
            .plugin_parameters(PluginInstanceId(71_001))
            .is_some()
    );
    let (minus6, _) = peak(&p, Some(SongId(70_000)));
    let ratio = minus6 / plain;
    assert!(
        (ratio - 0.501).abs() < 0.01,
        "−6 dB after the master: {ratio}"
    );
    let (minus60, _) = peak(&p, Some(SongId(70_001)));
    assert!(minus60 < plain * 0.002, "{minus60}");
    // A song that is gone monitors nothing.
    let (gone, _) = peak(&p, Some(SongId(1)));
    assert_eq!(gone, plain);
}
