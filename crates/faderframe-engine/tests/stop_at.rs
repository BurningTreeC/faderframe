//! A stop on an exact frame (a setlist song's end): playback stops there
//! whatever the blocks, once.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_transport::TransportCommand;

const SR: u32 = 48_000;

#[test]
fn playback_stops_on_the_frame_asked() {
    let mut tp = TestProject::new(SR);
    tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    // 256-frame blocks; the stop falls inside one.
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.controller.stop_at(Some(1000)).unwrap();
    r.play_from(0).unwrap();
    for _ in 0..8 {
        r.step();
    }
    let t = r.controller.transport_snapshot();
    assert!(!t.playing, "stopped");
    assert_eq!(t.position, 1000, "on the frame");
    // Once: playing on goes on.
    r.controller.transport(TransportCommand::Play).unwrap();
    for _ in 0..8 {
        r.step();
    }
    let t = r.controller.transport_snapshot();
    assert!(t.playing);
    assert!(t.position > 1000 + 256 * 6);
}
