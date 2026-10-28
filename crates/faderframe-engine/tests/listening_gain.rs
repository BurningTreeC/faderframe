//! The listening gain: the monitor path's one gain, per side — the mix (a
//! candidate Mix Intelligence auditions at the original's loudness) and
//! the compare source (a reference or the past at the mix's). It ramps
//! over a block, needs no rebuild, follows the side heard, and a render
//! made without it is as before.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::{PAGE_FRAMES, StreamSource, WavFormat, write_wav};
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::nodes::{CompareSettings, CompareSource};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_realtime::{Epoch, Reclaimer};
use faderframe_timeline::MusicalTime;

fn dc_project(value: f32) -> TestProject {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "DC", ChannelLayout::Stereo);
    let src = tp.dc(2, value, 96_000);
    tp.clip(t, src, MusicalTime::ZERO, 96_000);
    tp
}

#[test]
fn the_mix_side_gain_scales_what_is_heard_and_ramps() {
    let tp = dc_project(0.5);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let plain = r.render(4096);
    let level = plain[0][4000];
    assert!(level.abs() > 0.1, "{level}");
    // −6.02 dB on the mix: half, at once (no rebuild), reached within a
    // block.
    r.controller.set_listening_gain(-6.0206, 0.0);
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
    r.controller.set_listening_gain(f32::NAN, f32::INFINITY);
    let back = r.render(4096);
    assert!((back[0][4000] - level).abs() < 1e-5);
}

#[test]
fn each_side_is_heard_at_its_own_gain() {
    let tp = dc_project(0.5);
    // The other side: a constant 0.25 in a file.
    let dir = std::env::temp_dir().join(format!("ff-listening-gain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("other.wav");
    let n = PAGE_FRAMES * 2;
    write_wav(
        &path,
        &[vec![0.25; n], vec![0.25; n]],
        48_000,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let src = StreamSource::open(&path).unwrap();
    src.ensure(
        src.page_range(0, n as i64),
        &Epoch::new(),
        &mut Reclaimer::default(),
        &mut Vec::new(),
    )
    .unwrap();
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    let link = std::sync::Arc::clone(r.controller.listening().link.compare());
    link.set(Some(CompareSource {
        source: src,
        settings: CompareSettings::follow(0, n as i64),
    }));
    r.play_from(0).unwrap();
    let mix = r.render(4096)[0][4000];
    // The mix 6 dB down, the other side 6 dB up: each heard as set.
    r.controller.set_listening_gain(-6.0206, 6.0206);
    let quieter = r.render(4096)[0][4000];
    assert!((quieter - 0.5 * mix).abs() < 1e-4, "{quieter} {mix}");
    link.shared().hear(true);
    let other = r.render(8192);
    assert!((other[0][8000] - 0.5).abs() < 1e-3, "{}", other[0][8000]);
    link.set(None);
    let _ = std::fs::remove_dir_all(&dir);
}
