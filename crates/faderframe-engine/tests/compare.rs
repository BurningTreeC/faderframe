#![allow(clippy::unwrap_used)]
//! The compare source in the monitor path: heard instead of the mix with a
//! short equal-power crossfade (no gap, no click), in step with the
//! transport through locates and loop wraps, at its gain, looping its own
//! region, and gone again — all without a rebuild.

mod common;

use common::TestProject;
use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_audio_files::{StreamSource, WavFormat, write_wav};
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::nodes::{CompareSettings, CompareSource, CompareSync};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_realtime::{Epoch, Reclaimer};
use faderframe_timeline::MusicalTime;
use faderframe_transport::{LoopRange, TransportCommand};
use std::sync::Arc;

const RATE: u32 = 48_000;
const BLOCK: usize = 256;
/// A file frame's value: exact in f32 (frames below 2^24).
const SCALE: f32 = 1.0 / 1_048_576.0;
const MIX: f32 = 0.25;

fn frame_of(v: f32) -> i64 {
    (v / SCALE).round() as i64
}

/// A file whose left channel counts its frames (the right one negated).
fn counting_file(dir: &std::path::Path, frames: usize) -> Arc<StreamSource> {
    let l: Vec<f32> = (0..frames).map(|i| i as f32 * SCALE).collect();
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    let path = dir.join("compare.wav");
    write_wav(&path, &[l, r], RATE, WavFormat::Float32, false).unwrap();
    let src = StreamSource::open(&path).unwrap();
    src.ensure(
        src.page_range(0, frames as i64),
        &Epoch::new(),
        &mut Reclaimer::default(),
        &mut Vec::new(),
    )
    .unwrap();
    src
}

/// A project whose mix is a steady `MIX` on both sides.
fn renderer() -> OfflineRenderer {
    let mut p = TestProject::new(RATE);
    let t = p.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let dc = p.dc(2, MIX, RATE as usize * 20);
    p.clip(t, dc, MusicalTime::ZERO, RATE as i64 * 20);
    let config = EngineConfig {
        sample_rate: RATE,
        ..EngineConfig::default()
    };
    OfflineRenderer::new(&p.project, &p.sources, config, BLOCK, 2).unwrap()
}

fn block(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers) -> (Vec<f32>, Vec<f32>) {
    r.processor.process_device(bufs);
    r.controller.collect_garbage();
    (bufs.output(0).to_vec(), bufs.output(1).to_vec())
}

/// Blocks until `n` frames: the left channel.
fn frames(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers, n: usize) -> Vec<f32> {
    let mut out = Vec::new();
    while out.len() < n {
        out.extend(block(r, bufs).0);
    }
    out
}

/// No sample jumps from the one before by more than a crossfade's step
/// can move it, and none falls silent (the mix steady, the file a slow
/// ramp).
fn smooth(x: &[f32], what: &str) {
    for (i, w) in x.windows(2).enumerate() {
        assert!((w[1] - w[0]).abs() < 2e-3, "{what}: jump at {i}: {w:?}");
    }
    assert!(x.iter().all(|v| v.abs() > 0.05), "{what}: a gap");
}

#[test]
fn the_compare_source_takes_the_place_of_the_mix_in_step() {
    let dir = std::env::temp_dir().join(format!("ff-engine-compare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Frames well above the mix's level (0.25 … 0.6).
    let total = RATE as usize * 13;
    let src = counting_file(&dir, total);
    let anchor = -(RATE as i64 * 5);
    let mut r = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.compare());
    let shared = Arc::clone(link.shared());
    r.play_from(0).unwrap();
    let mix = frames(&mut r, &mut bufs, BLOCK * 8);
    let m = *mix.last().unwrap();
    assert!((m - MIX).abs() < 1e-6, "the mix: {m}");

    // Set, not heard yet: the mix as before.
    let generation = link.set(Some(CompareSource {
        source: Arc::clone(&src),
        settings: CompareSettings::follow(anchor, total as i64),
    }));
    let (a, _) = block(&mut r, &mut bufs);
    assert!(a.iter().all(|v| *v == m), "the mix only");
    assert_eq!(shared.installed(), generation);

    // Heard: an equal-power crossfade, then the file at the timeline's
    // position less the anchor, frame for frame.
    shared.hear(true);
    let fade = (RATE as f64 * faderframe_engine::nodes::compare::FADE_SECONDS) as usize;
    let x = frames(&mut r, &mut bufs, fade + BLOCK * 4);
    smooth(&x, "to the source");
    let end = r.controller.transport_snapshot().position;
    let n = x.len();
    for (i, v) in x[fade + 1..].iter().enumerate() {
        let at = end - (n - fade - 1 - i) as i64;
        assert_eq!(frame_of(*v), at - anchor, "frame {i} after the fade");
    }
    assert!((shared.heard() - 1.0).abs() < 1e-6);
    // The right channel is the file's right.
    let (l, rr) = block(&mut r, &mut bufs);
    assert!(l.iter().zip(&rr).all(|(a, b)| *a == -*b));

    // A locate: the next block plays from there on both sides.
    r.controller
        .transport(TransportCommand::Locate(RATE as i64 * 2))
        .unwrap();
    let (a, _) = block(&mut r, &mut bufs);
    for (i, v) in a.iter().enumerate() {
        assert_eq!(
            frame_of(*v),
            RATE as i64 * 2 + i as i64 - anchor,
            "after the locate"
        );
    }

    // A loop wrapping inside a block: on the very frame.
    let (ls, le) = (RATE as i64 * 3, RATE as i64 * 3 + 1000);
    r.controller
        .transport(TransportCommand::SetLoopRange(LoopRange::new(ls, le)))
        .unwrap();
    r.controller
        .transport(TransportCommand::SetLoopEnabled(true))
        .unwrap();
    r.controller
        .transport(TransportCommand::Locate(ls + 900))
        .unwrap();
    let a = frames(&mut r, &mut bufs, BLOCK * 2);
    let expected: Vec<i64> = (0..a.len() as i64)
        .map(|i| {
            let p = ls + 900 + i;
            (if p >= le { p - le + ls } else { p }) - anchor
        })
        .collect();
    let got: Vec<i64> = a.iter().map(|v| frame_of(*v)).collect();
    assert_eq!(got, expected, "through the loop's wrap");
    r.controller
        .transport(TransportCommand::SetLoopEnabled(false))
        .unwrap();

    // Its gain (level matching) reaches it within a block.
    shared.set_gain(0.5);
    block(&mut r, &mut bufs);
    let a = block(&mut r, &mut bufs).0;
    let end = r.controller.transport_snapshot().position;
    for (i, v) in a.iter().enumerate() {
        let f = end - (BLOCK - i) as i64 - anchor;
        assert!((v - 0.5 * f as f32 * SCALE).abs() < 1e-6, "at half");
    }
    shared.set_gain(1.0);
    block(&mut r, &mut bufs);

    // Back to the mix: crossfaded again, then exactly the mix.
    shared.hear(false);
    let x = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    smooth(&x, "back to the mix");
    assert!(x[fade + 1..].iter().all(|v| *v == m), "the mix again");

    // Looping its own region, whatever the timeline does.
    let (from, to) = (10_000i64, 10_700i64);
    link.set(Some(CompareSource {
        source: Arc::clone(&src),
        settings: CompareSettings {
            sync: CompareSync::Loop,
            from,
            to,
        },
    }));
    shared.hear(true);
    let x = frames(&mut r, &mut bufs, fade + BLOCK * 6);
    let got: Vec<i64> = x[fade + 1..].iter().map(|v| frame_of(*v)).collect();
    for w in got.windows(2) {
        assert!(
            w[1] == w[0] + 1 || (w[0] == to - 1 && w[1] == from),
            "the region round and round: {w:?}"
        );
    }
    assert!(got.iter().all(|f| (from..to).contains(f)));
    // A locate does not move it.
    let before = *got.last().unwrap();
    r.controller
        .transport(TransportCommand::Locate(RATE as i64))
        .unwrap();
    let a = block(&mut r, &mut bufs).0;
    let next = if before + 1 == to { from } else { before + 1 };
    assert_eq!(frame_of(a[0]), next);

    // Another source while heard: crossfaded from one to the other.
    link.set(Some(CompareSource {
        source: Arc::clone(&src),
        settings: CompareSettings::follow(anchor, total as i64),
    }));
    let x = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    for w in x.windows(2) {
        // The loop's region (≈ 0.01) to the timeline's frame (≈ 0.27).
        assert!((w[1] - w[0]).abs() < 2e-3, "a click between sources: {w:?}");
    }

    // Taken away while heard: back to the mix first, never a gap.
    link.set(None);
    let x = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    smooth(&x, "the source taken away");
    assert!(x[fade + 1..].iter().all(|v| *v == m));
    assert!(!shared.hearing());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_listening_graph_starts_with_the_compare_source() {
    let dir = std::env::temp_dir().join(format!("ff-engine-compare-new-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let total = RATE as usize * 4;
    let src = counting_file(&dir, total);
    let mut r = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.compare());
    link.set(Some(CompareSource {
        source: Arc::clone(&src),
        settings: CompareSettings::follow(0, total as i64),
    }));
    link.shared().hear(true);
    r.play_from(1000).unwrap();
    // The first block: the source at once (nothing was heard before).
    let (a, _) = block(&mut r, &mut bufs);
    for (i, v) in a.iter().enumerate() {
        assert_eq!(frame_of(*v), 1000 + i as i64);
    }
    // While stopped: silence from it (the transport plays nothing).
    r.controller.transport(TransportCommand::Stop).unwrap();
    let (a, _) = block(&mut r, &mut bufs);
    assert!(a.iter().all(|v| *v == 0.0), "stopped: {:?}", &a[..4]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn behind_latency_the_compare_source_meets_the_mix_on_the_same_frame() {
    use faderframe_audio_files::AudioData;
    use faderframe_core::{ParameterId, builtin};
    use faderframe_project::{PluginRef, PluginSlot, SavedParameter};
    let dir = std::env::temp_dir().join(format!("ff-engine-compare-pdc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let total = RATE as usize * 3;
    let src = counting_file(&dir, total);
    // The mix counts its frames too, 3000 frames late through a probe.
    let mut p = TestProject::new(RATE);
    let t = p.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let l: Vec<f32> = (0..total).map(|i| i as f32 * SCALE).collect();
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    let s = p.source(AudioData::from_channels(RATE, vec![l, r]));
    p.clip(t, s, MusicalTime::ZERO, total as i64);
    let id = p.project.ids.allocate();
    p.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id,
        plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(0),
            value: 3000.0,
        }],
        state: None,
        sidechain: None,
    });
    let config = EngineConfig {
        sample_rate: RATE,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&p.project, &p.sources, config, BLOCK, 2).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.compare());
    link.set(Some(CompareSource {
        source: Arc::clone(&src),
        settings: CompareSettings::follow(0, total as i64),
    }));
    r.play_from(0).unwrap();
    let mix = frames(&mut r, &mut bufs, BLOCK * 30);
    let last = frame_of(*mix.last().unwrap());
    assert!(last > 1000, "the mix plays: {last}");
    link.shared().hear(true);
    let fade = (RATE as f64 * faderframe_engine::nodes::compare::FADE_SECONDS) as usize;
    let x = frames(&mut r, &mut bufs, fade + BLOCK * 4);
    for (i, v) in x.iter().enumerate().skip(fade + 1) {
        assert_eq!(frame_of(*v), last + 1 + i as i64, "frame {i}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_looping_source_keeps_its_time_while_the_mix_is_heard() {
    let dir = std::env::temp_dir().join(format!("ff-engine-compare-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let total = RATE as usize * 2;
    let src = counting_file(&dir, total);
    let mut r = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.compare());
    let shared = Arc::clone(link.shared());
    let (from, to) = (20_000i64, 27_000i64);
    link.set(Some(CompareSource {
        source: Arc::clone(&src),
        settings: CompareSettings {
            sync: CompareSync::Loop,
            from,
            to,
        },
    }));
    shared.hear(true);
    r.play_from(0).unwrap();
    let a = frames(&mut r, &mut bufs, BLOCK * 4);
    let last = frame_of(*a.last().unwrap());
    // The mix for a while (crossfades both ways included), then back.
    shared.hear(false);
    let away = BLOCK * 40;
    frames(&mut r, &mut bufs, away);
    shared.hear(true);
    let fade = (RATE as f64 * faderframe_engine::nodes::compare::FADE_SECONDS) as usize;
    let b = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    let span = to - from;
    let expected = from + (last - from + (away + b.len()) as i64).rem_euclid(span);
    assert_eq!(frame_of(*b.last().unwrap()), expected);
    let next = if expected + 1 == to {
        from
    } else {
        expected + 1
    };
    assert_eq!(shared.position(), next);
    let _ = std::fs::remove_dir_all(&dir);
}
