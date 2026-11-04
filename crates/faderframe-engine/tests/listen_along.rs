#![allow(clippy::unwrap_used)]
//! Listen Along: another session's mix written into the listening node's
//! ring is heard in place of this one's, primed against jitter, faded in
//! and out; a dry ring is an underrun; the shared master is written to the
//! share ring for a host to stream.

mod common;

use common::TestProject;
use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_audio_files::AudioData;
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_timeline::MusicalTime;

const BLOCK: usize = 256;
const RATE: u32 = 48_000;

/// A project whose master plays a constant 0.25 on both sides (and the
/// master's id).
fn renderer() -> (OfflineRenderer, faderframe_core::TrackId) {
    let mut p = TestProject::new(RATE);
    let t = p.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let n = RATE as usize * 4;
    let src = p.source(AudioData::from_channels(
        RATE,
        vec![vec![0.25; n], vec![0.25; n]],
    ));
    p.clip(t, src, MusicalTime::ZERO, n as i64);
    let config = EngineConfig {
        sample_rate: RATE,
        ..EngineConfig::default()
    };
    let master = p.project.master_id().unwrap();
    (
        OfflineRenderer::new(&p.project, &p.sources, config, BLOCK, 2).unwrap(),
        master,
    )
}

fn block(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers) -> (Vec<f32>, Vec<f32>) {
    r.processor.process_device(bufs);
    r.controller.collect_garbage();
    (bufs.output(0).to_vec(), bufs.output(1).to_vec())
}

#[test]
fn the_stream_is_heard_in_place_of_the_mix_once_primed() {
    let (mut r, _) = renderer();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    for _ in 0..8 {
        block(&mut r, &mut bufs);
    }
    let (l, _) = block(&mut r, &mut bufs);
    assert!(l.iter().all(|v| (v - 0.25).abs() < 1e-3), "the own mix");
    let link = std::sync::Arc::clone(r.controller.listening().link.along());
    let shared = std::sync::Arc::clone(link.shared());
    shared.set_prime(4096);
    let mut feed = link.open(RATE as usize);
    shared.set_hear(true);
    // Not primed yet: still the own mix.
    feed.push(&[0.5; 512], &[-0.5; 512]);
    let (l, _) = block(&mut r, &mut bufs);
    assert!(
        l.iter().all(|v| (v - 0.25).abs() < 1e-3),
        "waits for the prime"
    );
    feed.push(&[0.5; 6000], &[-0.5; 6000]);
    // Faded over 30 ms, then the stream alone.
    for _ in 0..8 {
        block(&mut r, &mut bufs);
    }
    let (l, rr) = block(&mut r, &mut bufs);
    assert!(l.iter().all(|v| (v - 0.5).abs() < 1e-3), "{:?}", &l[..4]);
    assert!(rr.iter().all(|v| (v + 0.5).abs() < 1e-3));
    assert!(shared.playing());
    // It runs dry: an underrun, silence, then primed again.
    for _ in 0..40 {
        block(&mut r, &mut bufs);
    }
    assert!(shared.underruns() >= 1);
    let (l, _) = block(&mut r, &mut bufs);
    assert!(
        l.iter().all(|v| v.abs() < 1e-3),
        "dry: silence, not the own mix"
    );
    // Off: back to the own mix.
    shared.set_hear(false);
    for _ in 0..8 {
        block(&mut r, &mut bufs);
    }
    let (l, _) = block(&mut r, &mut bufs);
    assert!(l.iter().all(|v| (v - 0.25).abs() < 1e-3));
    assert!(!shared.playing());
}

#[test]
fn the_shared_master_reaches_the_share_ring() {
    let (mut r, master) = renderer();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    block(&mut r, &mut bufs);
    assert_eq!(r.controller.share().written(), 0, "nothing shared");
    r.controller.set_share_source(Some(master));
    for _ in 0..4 {
        block(&mut r, &mut bufs);
    }
    let (mut l, mut rr) = (Vec::new(), Vec::new());
    let (pos, _) = r.controller.share().read_since(0, &mut l, &mut rr);
    assert_eq!(pos, 4 * BLOCK as u64);
    assert!(l.iter().all(|v| (v - 0.25).abs() < 1e-3));
}
