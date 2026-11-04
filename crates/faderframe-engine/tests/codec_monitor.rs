#![allow(clippy::unwrap_used)]
//! The codec monitor in the monitor path: the decoded master, the original
//! and their difference heard a fixed delay later, switched without a
//! jump in time; on and off crossfaded; the platform's gain; what it
//! measures; and the heard clock counting its delay.

mod common;

use common::TestProject;
use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_audio_files::AudioData;
use faderframe_codec::{CodecSettings, CoderKind};
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::nodes::{CodecHear, CodecWanted};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::TrackKind;
use faderframe_timeline::MusicalTime;
use std::sync::Arc;

const BLOCK: usize = 256;
/// A frame's value: exact in f32 (frames below 2^24).
const SCALE: f32 = 1.0 / 1_048_576.0;

fn frame_of(v: f32) -> i64 {
    (v / SCALE).round() as i64
}

/// A project whose master plays `left`/`right` from the start.
fn renderer(rate: u32, left: Vec<f32>, right: Vec<f32>) -> OfflineRenderer {
    let mut p = TestProject::new(rate);
    let t = p.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let len = left.len() as i64;
    let src = p.source(AudioData::from_channels(rate, vec![left, right]));
    p.clip(t, src, MusicalTime::ZERO, len);
    let config = EngineConfig {
        sample_rate: rate,
        ..EngineConfig::default()
    };
    OfflineRenderer::new(&p.project, &p.sources, config, BLOCK, 2).unwrap()
}

/// The left channel counts its frames, the right one negated.
fn counting(rate: u32, seconds: usize) -> OfflineRenderer {
    let n = rate as usize * seconds;
    let l: Vec<f32> = (0..n).map(|i| i as f32 * SCALE).collect();
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    renderer(rate, l, r)
}

fn tone(rate: u32, seconds: usize, amp: f32) -> OfflineRenderer {
    let n = rate as usize * seconds;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / rate as f32;
            amp * (0.7 * (std::f32::consts::TAU * 330.0 * t).sin()
                + 0.3 * (std::f32::consts::TAU * 2_750.0 * t).sin())
        })
        .collect();
    renderer(rate, l.clone(), l)
}

/// One block: both outputs, and the timeline frame of its first frame.
fn block(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers) -> (Vec<f32>, Vec<f32>, i64) {
    let at = r.controller.transport_snapshot().position;
    r.processor.process_device(bufs);
    r.controller.collect_garbage();
    (bufs.output(0).to_vec(), bufs.output(1).to_vec(), at)
}

/// Blocks until `n` frames: the left channel and the timeline frame of
/// each of its frames.
fn frames(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers, n: usize) -> (Vec<f32>, Vec<i64>) {
    let (mut out, mut at) = (Vec::new(), Vec::new());
    while out.len() < n {
        let (l, _, p) = block(r, bufs);
        at.extend((0..l.len()).map(|i| p + i as i64));
        out.extend(l);
    }
    (out, at)
}

fn wanted(kind: CoderKind, hear: CodecHear, gain_db: f32) -> CodecWanted {
    CodecWanted {
        settings: CodecSettings {
            kind,
            kbps: 128,
            vbr: true,
        },
        hear,
        gain_db,
    }
}

fn fade_frames(rate: u32) -> usize {
    (f64::from(rate) * faderframe_engine::nodes::codec::FADE_SECONDS) as usize
}

#[test]
fn codec_original_and_difference_are_heard_in_step() {
    let rate = 48_000;
    let mut r = counting(rate, 20);
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.codec());
    let shared = Arc::clone(link.shared());
    r.play_from(0).unwrap();
    // The mix as it is: each frame its timeline frame.
    let (mix, at) = frames(&mut r, &mut bufs, BLOCK * 4);
    for (v, p) in mix.iter().zip(&at) {
        assert_eq!(frame_of(*v), *p);
    }
    assert_eq!(r.controller.listening_delay(), 0);

    // On: the mix goes on while the chain warms up, then crossfades to the
    // monitor, which plays every frame `delay` late.
    link.set(Some(wanted(CoderKind::Identity, CodecHear::Codec, 0.0)))
        .unwrap();
    let delay = faderframe_codec::FRAME + faderframe_codec::chain::IDENTITY_LOOKAHEAD;
    let fade = fade_frames(rate);
    let (x, at) = frames(&mut r, &mut bufs, delay + fade + BLOCK * 4);
    assert!(shared.installed());
    for (i, (v, p)) in x.iter().zip(&at).enumerate() {
        if i + BLOCK < delay {
            assert_eq!(frame_of(*v), *p, "warming up: the mix ({i})");
        }
    }
    let n = x.len();
    for i in n - BLOCK * 3..n {
        assert_eq!(frame_of(x[i]), at[i] - delay as i64, "the monitor at {i}");
    }
    assert_eq!(shared.heard(), 1.0);
    assert_eq!(r.controller.listening_delay(), delay as u32);
    assert_eq!(
        r.controller.heard_latency(),
        r.controller.device_latency() + delay as u32
    );
    // The right channel is the right one, delayed alike.
    let (l, rr, _) = block(&mut r, &mut bufs);
    assert!(l.iter().zip(&rr).all(|(a, b)| *a == -*b));

    // The original: the same frames (nothing coded), no jump.
    shared.set_hear(CodecHear::Original);
    let (x, at) = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    for (v, p) in x.iter().zip(&at) {
        assert_eq!(frame_of(*v), p - delay as i64, "original");
    }
    // The difference: nothing (the identity coder changes nothing).
    shared.set_hear(CodecHear::Difference);
    let (x, _) = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    assert!(x[fade + 1..].iter().all(|v| *v == 0.0), "difference");
    let reading = shared.reading();
    assert!(reading.frames > 0);
    assert_eq!(reading.difference_db(), Some(f64::NEG_INFINITY));
    // At a platform's gain (−6 dB): the decoded signal scaled.
    shared.set_hear(CodecHear::Codec);
    shared.set_gain(0.5);
    let (x, at) = frames(&mut r, &mut bufs, fade + BLOCK * 2);
    for i in fade + 1..x.len() {
        let want = (at[i] - delay as i64) as f32 * SCALE * 0.5;
        assert!(
            (x[i] - want).abs() <= want.abs() * 1e-6 + 1e-9,
            "gain at {i}"
        );
    }

    // Off: back to the mix as it is (crossfaded), the delay gone, the
    // chain handed back.
    link.set(None).unwrap();
    let (x, at) = frames(&mut r, &mut bufs, fade + BLOCK * 3);
    for i in x.len() - BLOCK * 2..x.len() {
        assert_eq!(frame_of(x[i]), at[i], "the mix again at {i}");
    }
    assert_eq!(r.controller.listening_delay(), 0);
    assert!(!shared.installed());
    link.collect();
}

#[test]
fn switching_on_and_off_never_clicks() {
    let rate = 48_000;
    let mut r = tone(rate, 10, 0.5);
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.codec());
    r.play_from(0).unwrap();
    frames(&mut r, &mut bufs, BLOCK * 4);
    link.set(Some(wanted(CoderKind::Identity, CodecHear::Codec, 0.0)))
        .unwrap();
    let (on, _) = frames(&mut r, &mut bufs, rate as usize / 4);
    link.set(None).unwrap();
    let (off, _) = frames(&mut r, &mut bufs, rate as usize / 8);
    // A tone of at most 0.5 moves less than 0.1 a frame; a cut would jump.
    for (what, x) in [("on", &on), ("off", &off)] {
        for (i, w) in x.windows(2).enumerate() {
            assert!((w[1] - w[0]).abs() < 0.12, "{what}: jump at {i}: {w:?}");
        }
    }
}

#[test]
fn at_44_1_khz_the_resampled_way_nulls_against_the_original() {
    let rate = 44_100;
    let mut r = tone(rate, 6, 0.5);
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.codec());
    let shared = Arc::clone(link.shared());
    r.play_from(0).unwrap();
    frames(&mut r, &mut bufs, BLOCK * 2);
    link.set(Some(wanted(CoderKind::Identity, CodecHear::Codec, 0.0)))
        .unwrap();
    frames(&mut r, &mut bufs, rate as usize / 2);
    let delay = r.controller.listening_delay() as usize;
    assert!(delay > faderframe_codec::FRAME, "{delay}");
    // The decoded signal lines up with the original to the sample: their
    // difference is the resamplers' residue, far down.
    let (codec, _) = frames(&mut r, &mut bufs, rate as usize / 2);
    shared.set_hear(CodecHear::Difference);
    frames(&mut r, &mut bufs, BLOCK * 8);
    let (diff, _) = frames(&mut r, &mut bufs, rate as usize / 2);
    let energy = |x: &[f32]| x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    let db = 10.0 * (energy(&diff) / energy(&codec)).log10();
    assert!(db < -70.0, "the difference is {db:.1} dB");
    let reading = shared.reading();
    assert!(reading.difference_db().unwrap() < -70.0);
}

#[cfg(not(faderframe_check_only))]
#[test]
fn opus_in_the_monitor_path_is_measured() {
    let rate = 48_000;
    // Peaks at 0.9: +6 dB of a platform's gain takes them over full scale.
    let mut r = tone(rate, 8, 0.9);
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.codec());
    let shared = Arc::clone(link.shared());
    r.play_from(0).unwrap();
    let (mix, _) = frames(&mut r, &mut bufs, BLOCK * 2);
    assert!(mix.iter().any(|v| v.abs() > 0.5));
    link.set(Some(wanted(CoderKind::Opus, CodecHear::Original, 0.0)))
        .unwrap();
    frames(&mut r, &mut bufs, rate as usize / 2);
    let delay = r.controller.listening_delay() as usize;
    assert_eq!(delay, faderframe_codec::FRAME + 312);
    // Original: the mix, `delay` late, exactly.
    let (orig, _) = frames(&mut r, &mut bufs, BLOCK * 4);
    // The codec: near it, not the same.
    shared.set_hear(CodecHear::Codec);
    shared.reset_measurements();
    frames(&mut r, &mut bufs, BLOCK * 8);
    let reading = shared.reading();
    assert!(reading.frames > 0);
    let d = reading.difference_db().unwrap();
    assert!(d < -12.0 && d > -90.0, "the codec's difference: {d:.1} dB");
    assert!(reading.peak_dbtp() > -3.0 && reading.peak_dbtp() < 1.0);
    assert_eq!(reading.overs, 0);
    assert!(orig.iter().any(|v| v.abs() > 0.5));
    // Turned up by 6 dB: over full scale, counted.
    shared.set_gain(2.0);
    shared.reset_measurements();
    frames(&mut r, &mut bufs, BLOCK * 16);
    let reading = shared.reading();
    assert!(reading.peak_dbtp() > 3.0, "{}", reading.peak_dbtp());
    assert!(reading.overs > 100, "{}", reading.overs);
    assert_eq!(reading.failures, 0);
    // A lower bitrate on the way: no new chain, the same delay.
    shared.set_codec(48, false);
    frames(&mut r, &mut bufs, BLOCK * 8);
    assert_eq!(r.controller.listening_delay() as usize, delay);
    link.set(None).unwrap();
    frames(&mut r, &mut bufs, BLOCK * 8);
    link.collect();
}
