#![allow(clippy::unwrap_used)]
//! Disk streaming through the engine: streamed sources must sound exactly
//! like in-memory ones, follow rate differences, and never block.

mod common;

use common::TestProject;
use faderframe_audio_files::{AudioData, PAGE_FRAMES, WavFormat, write_wav};
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-engine-stream-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn config(rate: u32) -> EngineConfig {
    EngineConfig {
        sample_rate: rate,
        ..EngineConfig::default()
    }
}

/// Deterministic, non-repeating test signal.
fn signal(n: usize, seed: f32) -> Vec<f32> {
    (0..n)
        .map(|i| ((i as f32 * 0.0137 + seed).sin() * 0.4) + ((i % 97) as f32 / 970.0))
        .collect()
}

#[test]
fn streamed_playback_matches_memory_playback() {
    let rate = 48_000;
    let n = PAGE_FRAMES * 4 + 1234;
    let l = signal(n, 0.1);
    let r = signal(n, 1.7);
    let path = tmp("match.wav");
    write_wav(
        &path,
        &[l.clone(), r.clone()],
        rate,
        WavFormat::Float32,
        false,
    )
    .unwrap();

    let render = |streamed: bool| {
        let mut tp = TestProject::new(rate);
        let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
        let src = if streamed {
            tp.stream(&path)
        } else {
            tp.source(AudioData::from_channels(rate, vec![l.clone(), r.clone()]))
        };
        // Offset into the file so reads start mid-page.
        tp.clip_at(t, src, MusicalTime::QUARTER, 5_000, (n - 5_000) as i64);
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(rate), 333, 2).unwrap();
        r.play_from(0).unwrap();
        let out = r.render(n + 30_000);
        assert_eq!(r.stream_errors, 0);
        out
    };
    let mem = render(false);
    let disk = render(true);
    assert_eq!(mem.len(), disk.len());
    let max_diff = mem[0]
        .iter()
        .zip(&disk[0])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_diff == 0.0, "streamed output differs by {max_diff}");
    assert!(
        mem[0].iter().any(|s| s.abs() > 0.1),
        "test signal reached the output"
    );
}

#[test]
fn file_rate_differing_from_engine_rate_keeps_pitch() {
    // A 441 Hz tone recorded at 44.1 kHz, project at 44.1 kHz, engine at 48 kHz.
    let n = 44_100;
    let tone: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * 441.0 * i as f32 / 44_100.0).sin() * 0.5)
        .collect();
    let path = tmp("rate.wav");
    write_wav(&path, &[tone], 44_100, WavFormat::Float32, false).unwrap();
    let mut tp = TestProject::new(44_100);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let src = tp.stream(&path);
    tp.clip(t, src, MusicalTime::ZERO, n as i64);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(48_000), 512, 2).unwrap();
    r.play_from(0).unwrap();
    let out = r.render(48_000);
    // One second of output → 441 rising zero crossings.
    let mid = &out[0][2_400..45_600];
    let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    let expected = 441 * mid.len() / 48_000;
    assert!(
        (crossings as i64 - expected as i64).abs() <= 2,
        "{crossings} vs {expected}"
    );
}

#[test]
fn missing_pages_play_silence_and_are_counted() {
    let rate = 48_000;
    let path = tmp("miss.wav");
    write_wav(
        &path,
        &[vec![0.5; PAGE_FRAMES * 2]],
        rate,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut tp = TestProject::new(rate);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let src = tp.stream(&path);
    tp.clip(t, src, MusicalTime::ZERO, (PAGE_FRAMES * 2) as i64);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(rate), 256, 2).unwrap();
    r.play_from(0).unwrap();
    // Drive the processor directly: no loader has run, nothing is resident.
    let mut bufs = faderframe_audio::OwnedBuffers::new(2, 2, 256);
    r.processor.process_device(&mut bufs);
    r.processor.process_device(&mut bufs);
    assert!(bufs.output_ref(0).iter().all(|s| *s == 0.0));
    let plan = r.controller.stream_plan();
    assert!(plan.sources()[0].misses() > 0);
}
