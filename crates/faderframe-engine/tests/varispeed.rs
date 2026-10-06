//! Varispeed through the engine: the transport advances at the speed, the
//! device hears the song that much faster (higher), and switching it off
//! returns to the exact 1:1 path.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

#[test]
fn the_song_plays_at_the_speed() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Mono);
    let tone: Vec<f32> = (0..SR as usize * 4)
        .map(|i| (i as f64 * 500.0 * std::f64::consts::TAU / SR as f64).sin() as f32 * 0.5)
        .collect();
    let src = tp.source(AudioData::from_channels(SR, vec![tone]));
    tp.clip(t, src, MusicalTime::ZERO, SR as i64 * 4);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 480, 2).unwrap();
    r.controller.set_varispeed(true).unwrap();
    r.controller.set_speed(1.02);
    r.play_from(0).unwrap();
    let out = r.render(SR as usize * 2);
    // Two device seconds: the transport moved 2.04 song seconds.
    let pos = r.controller.transport_snapshot().position as f64;
    assert!((pos / (2.0 * SR as f64) - 1.02).abs() < 1e-3, "{pos}");
    // Heard 2 % higher: 510 rising zero crossings in the second second.
    let last = &out[0][SR as usize..];
    let ups = last
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    assert!((ups as f64 - 510.0).abs() <= 1.0, "{ups}");
    // Off: 1:1 again.
    r.controller.set_varispeed(false).unwrap();
    let before = r.controller.transport_snapshot().position;
    r.render(SR as usize);
    let moved = r.controller.transport_snapshot().position - before;
    assert!((moved - SR as i64).abs() <= 480, "{moved}");
}
