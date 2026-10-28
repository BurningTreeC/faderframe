//! The monitor trim: a listening-only gain on what the device plays of
//! the master (Mix Intelligence's loudness-matched comparisons). It ramps
//! over a block, needs no rebuild, and a render made without it is as
//! before.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_timeline::MusicalTime;

#[test]
fn the_monitor_trim_scales_what_is_heard_and_ramps() {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "DC", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, 96_000);
    tp.clip(t, src, MusicalTime::ZERO, 96_000);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let plain = r.render(4096);
    let level = plain[0][4000];
    assert!(level.abs() > 0.1, "{level}");
    // −6.02 dB: half, at once (no rebuild), reached within a block.
    r.controller.set_monitor_trim(-6.0206).unwrap();
    let trimmed = r.render(4096);
    assert!(
        (trimmed[0][4000] - 0.5 * level).abs() < 1e-4,
        "{}",
        trimmed[0][4000]
    );
    assert!((trimmed[1][4000] - 0.5 * level).abs() < 1e-4);
    // The first block ramps (no step).
    let steps: f32 = trimmed[0]
        .windows(2)
        .take(256)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(steps < 0.01, "ramped: {steps}");
    // Back to 0 dB; nonsense values are taken as 0 dB.
    r.controller.set_monitor_trim(f32::NAN).unwrap();
    let back = r.render(4096);
    assert!((back[0][4000] - level).abs() < 1e-5);
}
