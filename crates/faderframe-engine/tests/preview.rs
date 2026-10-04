#![allow(clippy::unwrap_used)]
//! Album playback: a file plays to the outputs instead of the project,
//! sample for sample, paused, located and to its end.

use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_audio_files::{PAGE_FRAMES, StreamSource, WavFormat, write_wav};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_realtime::{Epoch, Reclaimer};

const BLOCK: usize = 256;

fn signal(n: usize, seed: f32) -> Vec<f32> {
    (0..n)
        .map(|i| (i as f32 * 0.0137 + seed).sin() * 0.4)
        .collect()
}

fn block(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers) -> (Vec<f32>, Vec<f32>) {
    r.processor.process_device(bufs);
    (bufs.output(0).to_vec(), bufs.output(1).to_vec())
}

#[test]
fn the_album_plays_instead_of_the_project() {
    let rate = 48_000;
    let n = PAGE_FRAMES * 2 + 3_000;
    let (l, rr) = (signal(n, 0.1), signal(n, 1.9));
    let dir = std::env::temp_dir().join(format!("ff-engine-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("album.wav");
    write_wav(
        &path,
        &[l.clone(), rr.clone()],
        rate,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let src = StreamSource::open(&path).unwrap();
    let epoch = Epoch::new();
    src.ensure(
        src.page_range(0, n as i64),
        &epoch,
        &mut Reclaimer::default(),
        &mut Vec::new(),
    )
    .unwrap();

    let project = demo_project(rate);
    let sources = render_generated_sources(&project, rate);
    let config = EngineConfig {
        sample_rate: rate,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    for _ in 0..40 {
        r.processor.process_device(&mut bufs);
    }
    assert!(
        bufs.output(0).iter().any(|v| v.abs() > 1e-3),
        "the project plays"
    );

    // Set: the project falls silent, the album waits at its start.
    r.controller.set_preview(Some(src.clone())).unwrap();
    let (a, _) = block(&mut r, &mut bufs);
    assert!(a.iter().all(|v| *v == 0.0));
    assert!(r.controller.preview().is_active() && !r.controller.preview().is_playing());

    // Playing: the file, sample for sample.
    r.controller.preview().play(true);
    let mut got = (Vec::new(), Vec::new());
    for _ in 0..10 {
        let (a, b) = block(&mut r, &mut bufs);
        got.0.extend(a);
        got.1.extend(b);
    }
    assert_eq!(got.0, l[..10 * BLOCK]);
    assert_eq!(got.1, rr[..10 * BLOCK]);
    assert_eq!(r.controller.preview().position(), (10 * BLOCK) as i64);

    // Paused: silence, the position kept.
    r.controller.preview().play(false);
    let (a, _) = block(&mut r, &mut bufs);
    assert!(a.iter().all(|v| *v == 0.0));
    assert_eq!(r.controller.preview().position(), (10 * BLOCK) as i64);

    // Located across a page boundary, then on.
    let at = PAGE_FRAMES as i64 - 100;
    r.controller.preview().locate(at);
    r.controller.preview().play(true);
    let (a, _) = block(&mut r, &mut bufs);
    assert_eq!(a, l[at as usize..at as usize + BLOCK]);

    // To the end: what is left, then silence and stopped there.
    r.controller.preview().locate(n as i64 - 100);
    let (a, _) = block(&mut r, &mut bufs);
    assert_eq!(a[..100], l[n - 100..]);
    assert!(a[100..].iter().all(|v| *v == 0.0));
    assert!(!r.controller.preview().is_playing());
    assert_eq!(r.controller.preview().position(), n as i64);
    // Play again: from the start.
    r.controller.preview().play(true);
    let (a, _) = block(&mut r, &mut bufs);
    assert_eq!(a, l[..BLOCK]);

    // Unset: the project again.
    r.controller.set_preview(None).unwrap();
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    assert!(!r.controller.preview().is_active());
    assert!(
        bufs.output(0).iter().any(|v| v.abs() > 1e-3),
        "the project plays again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
