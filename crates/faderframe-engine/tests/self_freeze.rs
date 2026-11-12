#![allow(clippy::unwrap_used)]
//! Self-freezing in the engine: a track played from a cache of its chain's
//! output sounds exactly as its chain did — from the start, after a switch
//! while playing (the handover crossfades, sample-exact where both agree),
//! across loop wraps and locates behind latency, and back to the chain.

mod common;

use common::TestProject;
use faderframe_audio_files::{AudioData, WavFormat};
use faderframe_core::{ChannelLayout, ParameterId, TrackId, builtin};
use faderframe_engine::cache::{CacheState, CachedChain};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, NodeWork, Source};
use faderframe_project::{Impact, PluginRef, PluginSlot, Project, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;
use std::path::{Path, PathBuf};

const SR: u32 = 48_000;
const BLOCK: usize = 256;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

fn slot(tp: &mut TestProject, id: &str, params: &[(u32, f64)]) -> PluginSlot {
    PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: params
            .iter()
            .map(|&(p, value)| SavedParameter {
                id: ParameterId(p),
                value,
            })
            .collect(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    }
}

/// A stereo track of two sines through `chain`, and a mono track with
/// more latency (so the cached one is delayed further by compensation).
fn project(chain: &[(&str, &[(u32, f64)])]) -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let wet = tp.track(TrackKind::Audio, "Wet", ChannelLayout::Stereo);
    let frames = 3 * SR as usize;
    let sine = |hz: f32| -> Vec<f32> {
        (0..frames)
            .map(|i| 0.4 * (std::f32::consts::TAU * hz * i as f32 / SR as f32).sin())
            .collect()
    };
    let src = tp.source(AudioData::from_channels(SR, vec![sine(220.0), sine(331.0)]));
    tp.clip(wet, src, MusicalTime::ZERO, frames as i64);
    for &(id, params) in chain {
        let s = slot(&mut tp, id, params);
        tp.project.track_mut(wet).unwrap().inserts.push(s);
    }
    let other = tp.track(TrackKind::Audio, "Other", ChannelLayout::Mono);
    let imp = tp.impulse(1, 64);
    tp.clip(other, imp, MusicalTime::from_quarters_i(1), 64);
    let late = slot(&mut tp, builtin::LATENCY_PROBE, &[(0, 700.0)]);
    tp.project.track_mut(other).unwrap().inserts.push(late);
    (tp, wet)
}

/// The chain's output as the session caches it: the track alone into a
/// plain master, its latency kept at the front. Returns the file and that
/// latency.
fn render_cache(tp: &TestProject, wet: TrackId, frames: usize, name: &str) -> (PathBuf, u32) {
    let mut p = tp.project.clone();
    p.tracks
        .retain(|t| t.id == wet || t.kind == TrackKind::Master);
    p.clips.retain(|_, c| c.track == wet);
    let mut r = OfflineRenderer::new(&p, &tp.sources, config(), BLOCK, 2).unwrap();
    let file_latency = r.controller.graph_stats().output_latency;
    r.play_from(0).unwrap();
    let audio = r.render(frames);
    let path =
        std::env::temp_dir().join(format!("ff-self-freeze-{}-{name}.wav", std::process::id()));
    faderframe_audio_files::write_wav(&path, &audio, SR, WavFormat::Float32, false).unwrap();
    (path, file_latency)
}

/// The cache as the engine plays it (a stream of its own).
fn chain(path: &Path, file_latency: u32, latency: u32) -> CachedChain {
    CachedChain {
        source: Source::Stream(faderframe_audio_files::StreamSource::open(path).unwrap()),
        start: 0,
        file_latency,
        latency,
        key: 7,
    }
}

fn renderer(p: &Project, tp: &TestProject) -> OfflineRenderer {
    OfflineRenderer::new(p, &tp.sources, config(), BLOCK, 2).unwrap()
}

fn live_latency(tp: &TestProject, wet: TrackId) -> u32 {
    renderer(&tp.project, tp).controller.track_latency(wet)
}

fn append(out: &mut [Vec<f32>], more: Vec<Vec<f32>>) {
    for (o, m) in out.iter_mut().zip(more) {
        o.extend(m);
    }
}

/// Render callback by callback until `wet`'s handover is over (at most `max`
/// frames), then let the next graph drop the side no longer heard.
fn through_handover(
    r: &mut OfflineRenderer,
    p: &Project,
    tp: &TestProject,
    out: &mut [Vec<f32>],
    max: usize,
) {
    let mut done = 0;
    while done < max {
        append(out, r.render(BLOCK));
        done += BLOCK;
        if r.controller.settle_cache() {
            r.controller.sync(p, &tp.sources, Impact::Graph).unwrap();
            return;
        }
    }
    panic!("the handover did not end within {max} frames");
}

fn assert_same(got: &[Vec<f32>], want: &[Vec<f32>], what: &str) {
    for (c, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g.len(), w.len(), "{what}: lengths");
        if let Some(i) = g.iter().zip(w).position(|(a, b)| a != b) {
            panic!(
                "{what}: channel {c} differs from frame {i}: {} vs {}",
                g[i], w[i]
            );
        }
    }
}

fn devices_of(r: &OfflineRenderer, track: TrackId) -> usize {
    let profile = r.controller.graph_profile().unwrap();
    profile
        .owners
        .iter()
        .flatten()
        .filter(|o| o.track == track && o.work == NodeWork::Insert)
        .count()
}

const ECHO_CHAIN: &[(&str, &[(u32, f64)])] = &[
    (builtin::LATENCY_PROBE, &[(0, 300.0)]),
    (builtin::ECHO, &[]),
    (builtin::SATURATOR, &[]),
];
const DELAY_CHAIN: &[(&str, &[(u32, f64)])] = &[
    (builtin::GAIN, &[(0, -6.0)]),
    (builtin::LATENCY_PROBE, &[(0, 300.0)]),
];

#[test]
fn a_switch_to_the_cache_while_playing_is_sample_exact() {
    let (tp, wet) = project(ECHO_CHAIN);
    let total = 2 * SR as usize;
    let mut want = renderer(&tp.project, &tp);
    want.play_from(0).unwrap();
    let want = want.render(total);
    assert!(want[0].iter().any(|s| s.abs() > 0.1), "something to hear");
    let (path, file_latency) = render_cache(&tp, wet, total + SR as usize, "switch");
    let latency = live_latency(&tp, wet);
    assert!(latency > 300, "the chain's latency ({latency})");

    let mut r = renderer(&tp.project, &tp);
    r.play_from(0).unwrap();
    let mut got = r.render(BLOCK * 100);
    assert!(devices_of(&r, wet) > 0);
    assert!(
        r.controller
            .set_cached(wet, Some(chain(&path, file_latency, latency)))
    );
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    assert_eq!(r.controller.cache_state(wet), Some(CacheState::ToCache));
    through_handover(&mut r, &tp.project, &tp, &mut got, SR as usize / 2);
    assert_eq!(r.controller.cache_state(wet), Some(CacheState::Cached));
    assert_eq!(devices_of(&r, wet), 0, "the chain is gone");
    assert_eq!(
        r.controller.track_latency(wet),
        latency,
        "compensation as it was"
    );
    let left = total - got[0].len();
    append(&mut got, r.render(left));
    assert_same(&got, &want, "live, then cached");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_cache_played_from_the_start_is_the_chain() {
    let (tp, wet) = project(ECHO_CHAIN);
    let total = 2 * SR as usize;
    let mut want = renderer(&tp.project, &tp);
    want.play_from(0).unwrap();
    let want = want.render(total);
    let (path, file_latency) = render_cache(&tp, wet, total + SR as usize, "start");
    let latency = live_latency(&tp, wet);
    // Cached before playing: the handover ends while stopped.
    let mut r = renderer(&tp.project, &tp);
    r.controller
        .set_cached(wet, Some(chain(&path, file_latency, latency)));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let mut silence = vec![Vec::new(), Vec::new()];
    through_handover(&mut r, &tp.project, &tp, &mut silence, SR as usize / 2);
    assert!(silence.iter().flatten().all(|s| *s == 0.0));
    r.play_from(0).unwrap();
    let got = r.render(total);
    assert_same(&got, &want, "cached from the start");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn back_to_the_chain_without_a_jump() {
    // A chain whose memory is its latency: back from the cache, reset, it
    // has the same output once its latency is filled, so even the way
    // back is exact.
    let (tp, wet) = project(DELAY_CHAIN);
    let total = 2 * SR as usize;
    let mut want = renderer(&tp.project, &tp);
    want.play_from(0).unwrap();
    let want = want.render(total);
    let (path, file_latency) = render_cache(&tp, wet, total + SR as usize, "back");
    let latency = live_latency(&tp, wet);
    let mut r = renderer(&tp.project, &tp);
    r.play_from(0).unwrap();
    let mut got = r.render(BLOCK * 40);
    r.controller
        .set_cached(wet, Some(chain(&path, file_latency, latency)));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    through_handover(&mut r, &tp.project, &tp, &mut got, SR as usize / 2);
    append(&mut got, r.render(BLOCK * 60));
    assert!(r.controller.set_cached(wet, None));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    assert_eq!(r.controller.cache_state(wet), Some(CacheState::ToLive));
    assert!(devices_of(&r, wet) > 0, "the chain is back");
    through_handover(&mut r, &tp.project, &tp, &mut got, SR as usize / 2);
    assert_eq!(r.controller.cache_state(wet), None);
    let left = total - got[0].len();
    append(&mut got, r.render(left));
    assert_same(&got, &want, "live, cached, live");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_chain_with_a_long_memory_comes_back_gently() {
    // Nothing changed: the cache stays heard while the echo builds its
    // repeats up again, then the fade leaves no step.
    let (tp, wet) = project(ECHO_CHAIN);
    let total = 3 * SR as usize;
    let mut want = renderer(&tp.project, &tp);
    want.play_from(0).unwrap();
    let want = want.render(total);
    let (path, file_latency) = render_cache(&tp, wet, total + SR as usize, "gently");
    let latency = live_latency(&tp, wet);
    let mut r = renderer(&tp.project, &tp);
    r.controller
        .set_cached(wet, Some(chain(&path, file_latency, latency)));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let mut got = vec![Vec::new(), Vec::new()];
    through_handover(&mut r, &tp.project, &tp, &mut got, SR as usize / 2);
    got = vec![Vec::new(), Vec::new()];
    r.play_from(0).unwrap();
    // Whole callbacks: what `render` cuts off a callback is not heard
    // again.
    let played = BLOCK * 94;
    append(&mut got, r.render(played));
    assert!(r.controller.release_cache(&tp.project, wet));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let wait = r.controller.cache_handover(wet).unwrap().wait() as usize;

    assert!(
        wait >= SR as usize * 9 / 10,
        "the echo's tail first ({wait})"
    );
    through_handover(&mut r, &tp.project, &tp, &mut got, 2 * SR as usize);
    let left = total - got[0].len();
    append(&mut got, r.render(left));
    // The cache until the fade.
    let cut =
        |v: &[Vec<f32>], n: usize| -> Vec<Vec<f32>> { v.iter().map(|c| c[..n].to_vec()).collect() };
    let fade = played + wait;
    assert_same(
        &cut(&got, fade),
        &cut(&want, fade),
        "the cache while the chain warms",
    );
    let step = |s: &[f32]| {
        s.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    };
    for c in 0..2 {
        let around = fade - BLOCK..total;
        let got_step = step(&got[c][around.clone()]);
        let want_step = step(&want[c][around]);
        assert!(
            got_step <= want_step * 1.5 + 1e-3,
            "channel {c}: steps of {got_step} where the chain makes {want_step}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn loops_locates_and_stops_land_as_behind_the_chain() {
    // A chain whose output is its input delayed: what the cache plays
    // behind latency follows the play history exactly.
    let (mut tp, wet) = project(DELAY_CHAIN);
    let total = SR as usize * 3 / 2;
    tp.project.loop_range = Some(faderframe_project::MusicalRange {
        start: MusicalTime::from_quarters(0.5),
        end: MusicalTime::from_quarters_i(1),
    });
    tp.project.loop_enabled = true;
    let (path, file_latency) = render_cache(&tp, wet, 3 * SR as usize + SR as usize, "loop");
    let latency = live_latency(&tp, wet);
    let play = |r: &mut OfflineRenderer| {
        r.play_from(0).unwrap();
        let mut out = r.render(total / 2);
        r.controller
            .transport(TransportCommand::Locate(SR as i64 / 3))
            .unwrap();
        append(&mut out, r.render(total / 4));
        r.controller.transport(TransportCommand::Stop).unwrap();
        append(&mut out, r.render(total / 4));
        out
    };
    let want = play(&mut renderer(&tp.project, &tp));
    let mut r = renderer(&tp.project, &tp);
    r.controller
        .set_cached(wet, Some(chain(&path, file_latency, latency)));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let mut silence = vec![Vec::new(), Vec::new()];
    through_handover(&mut r, &tp.project, &tp, &mut silence, SR as usize / 2);
    let got = play(&mut r);
    // Until the stop: exact. After it the cache fades out in 10 ms where
    // the delay line would have played its last 300 frames.
    let stop = total / 2 + total / 4;
    let cut = |v: &[Vec<f32>]| -> Vec<Vec<f32>> { v.iter().map(|c| c[..stop].to_vec()).collect() };
    assert_same(&cut(&got), &cut(&want), "looped and located");
    let after = |v: &[Vec<f32>]| {
        v[0][stop + 2_000..]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    assert!(
        after(&got) < 1e-6 && after(&want) < 1e-6,
        "silent after the stop: {} {}",
        after(&got),
        after(&want)
    );
    let _ = std::fs::remove_file(&path);
}
