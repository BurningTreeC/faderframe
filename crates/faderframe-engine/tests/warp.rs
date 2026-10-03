//! Warped audio plays through its time map: varispeed changes pitch,
//! the stretch modes keep it, hits land where the warp markers put them,
//! and playback that starts mid-clip is primed seamlessly.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{AudioSourceId, ChannelLayout, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::{OfflineRenderer, render_project};
use faderframe_project::{ClipContent, Impact, TrackKind, Warp, WarpAlgorithm, WarpMarker};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn sine(freq: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (i as f64 * freq * std::f64::consts::TAU / SR as f64).sin() as f32 * 0.5)
        .collect()
}

/// Short decaying bursts at `at`.
fn clicks(at: &[usize], frames: usize) -> Vec<f32> {
    let mut x = vec![0.0f32; frames];
    let mut seed = 99u32;
    for &p in at {
        for k in 0..2_400 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
            if p + k < frames {
                x[p + k] += noise * 0.9 * (-(k as f32) / 300.0).exp();
            }
        }
    }
    x
}

fn frequency(x: &[f32]) -> f64 {
    let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    crossings as f64 * SR as f64 / x.len() as f64
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// A mono track with one clip of `samples` (warped by `warp`).
fn project(samples: Vec<f32>, length: i64, warp: Option<Warp>) -> (TestProject, TrackId, ClipId) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "warped", ChannelLayout::Mono);
    let src: AudioSourceId = tp.source(AudioData::from_channels(SR, vec![samples]));
    tp.clip(t, src, MusicalTime::ZERO, length);
    let clip = *tp.project.track(t).unwrap().clips.last().unwrap();
    if let Some(ClipContent::Audio(a)) = tp.project.clips.get_mut(&clip).map(|c| &mut c.content) {
        a.warp = warp;
    }
    (tp, t, clip)
}

fn uniform(source_length: i64, algorithm: WarpAlgorithm) -> Warp {
    Warp {
        source_length,
        markers: Vec::new(),
        algorithm,
    }
}

fn render(tp: &TestProject, from: i64, frames: usize) -> Vec<f32> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        from,
        frames,
    )
    .unwrap()
    .remove(0)
}

#[test]
fn varispeed_changes_pitch_and_stretching_keeps_it() {
    for (algorithm, expected) in [
        (WarpAlgorithm::Varispeed, 220.0),
        (WarpAlgorithm::Polyphonic, 440.0),
        (WarpAlgorithm::Rhythmic, 440.0),
    ] {
        // One second of 440 Hz played over two seconds.
        let (tp, ..) = project(
            sine(440.0, 48_000),
            96_000,
            Some(uniform(48_000, algorithm)),
        );
        let out = render(&tp, 0, 96_000);
        let mid = &out[24_000..72_000];
        let f = frequency(mid);
        assert!((f - expected).abs() < 4.0, "{algorithm:?}: {f} Hz");
        assert!(rms(mid) > 0.15, "{algorithm:?}: level {}", rms(mid));
        // It lasts the clip's length.
        assert!(rms(&out[90_000..95_000]) > 0.1, "{algorithm:?} ends early");
    }
}

#[test]
fn hits_land_where_the_markers_put_them() {
    let hits = [12_000, 24_000, 36_000];
    // Pin the outer hits, move the middle one 6000 frames later.
    let warp = Warp {
        source_length: 48_000,
        markers: vec![
            WarpMarker {
                at: 12_000,
                source: 12_000,
            },
            WarpMarker {
                at: 30_000,
                source: 24_000,
            },
            WarpMarker {
                at: 36_000,
                source: 36_000,
            },
        ],
        algorithm: WarpAlgorithm::Rhythmic,
    };
    let (tp, ..) = project(clicks(&hits, 48_000), 48_000, Some(warp));
    let out = render(&tp, 0, 48_000);
    for expected in [12_000usize, 30_000, 36_000] {
        // Where the energy of the hit is (±1000 frames around it).
        let window = &out[expected - 1_000..expected + 1_800];
        let peak = window
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap();
        let at = expected - 1_000 + peak.0;
        assert!(
            (at as i64 - expected as i64).abs() < 480,
            "hit expected at {expected}, loudest at {at}"
        );
        assert!(
            peak.1.abs() > 0.2,
            "hit at {expected} too quiet: {}",
            peak.1
        );
    }
    // Nothing where the middle hit used to be.
    assert!(
        rms(&out[23_800..25_000]) < 0.05,
        "{}",
        rms(&out[23_800..25_000])
    );
}

#[test]
fn starting_mid_clip_is_primed_and_locates_stay_seamless() {
    let (tp, ..) = project(
        sine(330.0, 48_000),
        72_000,
        Some(uniform(48_000, WarpAlgorithm::Polyphonic)),
    );
    let steady = rms(&render(&tp, 0, 60_000)[20_000..40_000]);
    // From the middle: full level right away, same pitch.
    let out = render(&tp, 30_000, 20_000);
    assert!(
        rms(&out[..1_024]) > steady * 0.7,
        "{} vs {steady}",
        rms(&out[..1_024])
    );
    assert!((frequency(&out[2_000..]) - 330.0).abs() < 5.0);
    // Locating again rebinds and re-primes the voice.
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 1).unwrap();
    r.play_from(10_000).unwrap();
    let _ = r.render(4_096);
    r.play_from(50_000).unwrap();
    let after = r.render(4_096).remove(0);
    assert!(
        rms(&after[..1_024]) > steady * 0.7,
        "after a locate: {}",
        rms(&after[..1_024])
    );
}

#[test]
fn warping_a_clip_later_gives_the_track_voices() {
    // Start unwarped; warping it is a timeline edit, which must rebuild the
    // graph so the track gets a stretcher (pitch stays at 440 Hz).
    let (mut tp, _, clip) = project(sine(440.0, 48_000), 48_000, None);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 1).unwrap();
    if let Some(ClipContent::Audio(a)) = tp.project.clips.get_mut(&clip).map(|c| &mut c.content) {
        a.length = 96_000;
        a.warp = Some(uniform(48_000, WarpAlgorithm::Polyphonic));
    }
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Timeline)
        .unwrap();
    r.play_from(0).unwrap();
    let out = r.render(96_000).remove(0);
    let f = frequency(&out[24_000..72_000]);
    assert!((f - 440.0).abs() < 4.0, "{f} Hz");
}
