//! Clip envelopes play sample by sample on top of clip gain and fades:
//! volume in dB along the segments' shapes, pan as a balance on stereo
//! tracks, the same whatever the block size, and they move with the clip.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_core::{ChannelLayout, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopePoint, FLOOR_DB};
use faderframe_project::{ClipContent, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

/// A stereo track playing a stereo DC clip of 0.5 from the song's start.
fn project(frames: i64) -> (TestProject, TrackId, ClipId) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "a", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, frames as usize);
    tp.clip(t, src, MusicalTime::ZERO, frames);
    let clip = *tp.project.track(t).unwrap().clips.last().unwrap();
    (tp, t, clip)
}

fn set_envelope(tp: &mut TestProject, clip: ClipId, e: ClipEnvelope) {
    if let Some(ClipContent::Audio(a)) = tp.project.clips.get_mut(&clip).map(|c| &mut c.content) {
        a.envelope = e.boxed();
    }
}

fn render(tp: &TestProject, frames: usize, block: usize) -> Vec<Vec<f32>> {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, block, 2).unwrap();
    r.play_from(0).unwrap();
    r.render(frames)
}

fn volume(points: &[(i64, f64)]) -> ClipEnvelope {
    ClipEnvelope {
        volume: points
            .iter()
            .map(|&(a, v)| EnvelopePoint::new(a, v))
            .collect(),
        pan: Vec::new(),
    }
}

#[test]
fn a_volume_envelope_rides_the_clip_sample_by_sample() {
    let (mut tp, _, clip) = project(96_000);
    let plain = render(&tp, 72_000, 256);
    set_envelope(
        &mut tp,
        clip,
        volume(&[(0, 0.0), (24_000, -20.0 * 2f64.log10()), (48_000, FLOOR_DB)]),
    );
    let enveloped = render(&tp, 72_000, 256);
    let ratio = |t: usize| enveloped[0][t] / plain[0][t];
    assert!((ratio(0) - 1.0).abs() < 1e-6);
    // Halfway down in dB: −3 dB.
    assert!(
        (ratio(12_000) - 0.5f32.sqrt()).abs() < 1e-4,
        "{}",
        ratio(12_000)
    );
    assert!((ratio(24_000) - 0.5).abs() < 1e-5, "{}", ratio(24_000));
    // At and after the floor point: silent.
    assert_eq!(enveloped[0][48_000], 0.0);
    assert_eq!(enveloped[1][60_000], 0.0);
    // Monotonic down the ramp, both channels alike.
    for t in (1..48_000).step_by(997) {
        assert!(ratio(t) <= ratio(t - 1) + 1e-7);
        assert_eq!(enveloped[0][t], enveloped[1][t]);
    }
}

#[test]
fn the_envelope_is_the_same_whatever_the_block() {
    let (mut tp, _, clip) = project(48_000);
    let mut e = volume(&[(1_000, -12.0), (1_001, 3.0), (20_000, -30.0)]);
    e.volume[1].shape = faderframe_automation::CurveShape::Smooth;
    e.pan = vec![
        EnvelopePoint::new(5_000, -0.5),
        EnvelopePoint::new(9_000, 0.8),
    ];
    set_envelope(&mut tp, clip, e);
    let a = render(&tp, 30_000, 256);
    let b = render(&tp, 30_000, 77);
    assert_eq!(a, b);
}

#[test]
fn a_pan_envelope_balances_a_stereo_clip() {
    let (mut tp, _, clip) = project(48_000);
    let plain = render(&tp, 30_000, 256);
    set_envelope(
        &mut tp,
        clip,
        ClipEnvelope {
            volume: Vec::new(),
            pan: vec![EnvelopePoint::new(0, -1.0), EnvelopePoint::new(24_000, 1.0)],
        },
    );
    let panned = render(&tp, 30_000, 256);
    let side = |c: usize, t: usize| panned[c][t] / plain[c][t];
    // Hard left: the right side is gone, the left kept.
    assert!((side(0, 0) - 1.0).abs() < 1e-6 && side(1, 0).abs() < 1e-6);
    // Centre halfway, both at unity.
    assert!((side(0, 12_000) - 1.0).abs() < 1e-4 && (side(1, 12_000) - 1.0).abs() < 1e-4);
    // A quarter right of centre: the left fades.
    assert!((side(0, 15_000) - 0.75).abs() < 1e-3, "{}", side(0, 15_000));
    assert!((side(1, 15_000) - 1.0).abs() < 1e-6);
    // Hard right after the last point.
    assert!(side(0, 28_000).abs() < 1e-6 && (side(1, 28_000) - 1.0).abs() < 1e-6);
}

#[test]
fn the_envelope_moves_with_its_clip() {
    let (mut tp, _, clip) = project(48_000);
    set_envelope(&mut tp, clip, volume(&[(0, FLOOR_DB), (1, 0.0)]));
    // One beat later (120 BPM: 24 000 frames).
    tp.project.clips.get_mut(&clip).unwrap().start = MusicalTime::from_quarters(1.0);
    let out = render(&tp, 30_000, 256);
    assert_eq!(out[0][23_999], 0.0, "before the clip");
    assert_eq!(out[0][24_000], 0.0, "the envelope's silent first point");
    assert!(out[0][24_001] > 0.4, "{}", out[0][24_001]);
}
