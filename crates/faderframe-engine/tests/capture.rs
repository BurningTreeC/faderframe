#![allow(clippy::unwrap_used)]
//! Capturing inside a render (Mix Intelligence's one pass): taps on the
//! strips hear each track as it reaches its destination, aligned with the
//! master by the graph's own delay compensation; probes after the devices
//! measure what each one does; an engine without a plan builds nothing.

mod common;

use common::{TestProject, hits};
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::capture::CapturePlan;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

fn device(
    tp: &mut TestProject,
    track: TrackId,
    id: &str,
    params: &[(u32, f64)],
) -> PluginInstanceId {
    let plugin = tp.project.ids.allocate();
    tp.project
        .track_mut(track)
        .unwrap()
        .inserts
        .push(PluginSlot {
            id: plugin,
            plugin: PluginRef::builtin(id, id),
            bypass: false,
            parameters: params
                .iter()
                .map(|&(id, value)| SavedParameter {
                    id: ParameterId(id),
                    value,
                })
                .collect(),
            state: None,
            sidechain: None,
        });
    plugin
}

/// The master and every tap over `frames`, the graph's latency taken off
/// the front of all of them alike.
struct Captured {
    master: Vec<Vec<f32>>,
    taps: Vec<(TrackId, Vec<Vec<f32>>)>,
    latency: usize,
}

fn capture(tp: &TestProject, plan: CapturePlan, frames: usize) -> (Captured, OfflineRenderer) {
    let mut r =
        OfflineRenderer::with_capture(&tp.project, &tp.sources, config(), BLOCK, 2, Some(plan))
            .unwrap();
    let latency = r.controller.graph_stats().output_latency as usize;
    let taps = r.controller.capture().unwrap().taps();
    let mut master = vec![Vec::new(); 2];
    let mut tapped: Vec<Vec<Vec<f32>>> = taps
        .iter()
        .map(|t| vec![Vec::new(); t.channels()])
        .collect();
    r.play_from(0).unwrap();
    while master[0].len() < frames + latency {
        let out = r.step();
        for (c, m) in master.iter_mut().enumerate() {
            m.extend_from_slice(out.output_ref(c));
        }
        for (t, dst) in taps.iter().zip(&mut tapped) {
            assert!(t.take(|chs| {
                assert_eq!(chs[0].len(), BLOCK, "a block per callback");
                for (d, c) in dst.iter_mut().zip(chs) {
                    d.extend_from_slice(c);
                }
            }));
        }
    }
    for t in &taps {
        assert_eq!(t.missed(), 0);
    }
    let cut = |v: &mut Vec<Vec<f32>>| {
        for c in v.iter_mut() {
            c.drain(..latency);
            c.truncate(frames);
        }
    };
    cut(&mut master);
    let taps = taps
        .iter()
        .zip(tapped)
        .map(|(t, mut v)| {
            cut(&mut v);
            (t.track(), v)
        })
        .collect();
    (
        Captured {
            master,
            taps,
            latency,
        },
        r,
    )
}

#[test]
fn taps_hear_each_track_where_the_master_hears_it() {
    let mut tp = TestProject::new(SR);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let b = tp.track(TrackKind::Audio, "B", ChannelLayout::Stereo);
    let c = tp.track(TrackKind::Audio, "C", ChannelLayout::Mono);
    let bus = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Stereo);
    tp.route(c, bus);
    let imp = tp.impulse(1, 4000);
    let imp2 = tp.impulse(2, 4000);
    // Each track's impulse at its own place on the timeline.
    tp.clip(a, imp, MusicalTime::ZERO, 4000);
    tp.clip(b, imp2, MusicalTime::from_quarters(0.5), 4000);
    tp.clip(c, imp, MusicalTime::from_quarters(1.0), 4000);
    // Unequal latency on the way: A's devices, the bus's.
    device(&mut tp, a, builtin::LATENCY_PROBE, &[(0, 300.0)]);
    device(&mut tp, bus, builtin::LATENCY_PROBE, &[(0, 77.0)]);
    let plan = CapturePlan {
        tracks: vec![a, b, c, bus],
        devices: false,
        block: 0,
    };
    let frames = 48_000;
    let (got, _) = capture(&tp, plan, frames);
    assert_eq!(got.latency, 300, "A's devices are the slowest path");
    let tap = |t: TrackId| &got.taps.iter().find(|(x, _)| *x == t).unwrap().1;
    // The master is the sum of what reaches it, sample for sample.
    for ch in 0..2 {
        for i in 0..frames {
            let sum = tap(a)[ch][i] + tap(b)[ch][i] + tap(bus)[ch][i];
            assert!(
                (got.master[ch][i] - sum).abs() < 1e-6,
                "channel {ch} frame {i}: {} vs {sum}",
                got.master[ch][i]
            );
        }
    }
    // Every tap hears its impulse where the master does: at its place on
    // the timeline, whatever latency its path has (C reaches the bus where
    // the bus passes it on).
    let q = 24_000 / 2;
    assert_eq!(hits(&tap(a)[0], 0.1), vec![0]);
    assert_eq!(hits(&tap(b)[0], 0.1), vec![q]);
    assert_eq!(hits(&tap(c)[0], 0.1), vec![2 * q]);
    assert_eq!(hits(&tap(bus)[0], 0.1), vec![2 * q]);
    assert_eq!(hits(&got.master[0], 0.1), vec![0, q, 2 * q]);
}

#[test]
fn probes_measure_what_each_device_does() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "T", ChannelLayout::Mono);
    let dc = tp.dc(1, 0.5, 24_000);
    tp.clip(t, dc, MusicalTime::ZERO, 24_000);
    use faderframe_plugin_host::devices::utility::id as utility;
    // +12 dB: 0.5 → 2.0, over full scale into the next device; that one
    // only delays (aligned: it changes nothing); the last one silences.
    let louder = device(&mut tp, t, builtin::GAIN, &[(utility::GAIN, 12.0412)]);
    let late = device(&mut tp, t, builtin::LATENCY_PROBE, &[(0, 64.0)]);
    let mute = device(&mut tp, t, builtin::GAIN, &[(utility::MUTE, 1.0)]);
    let plan = CapturePlan {
        tracks: vec![t],
        devices: true,
        block: 0,
    };
    let (_, r) = capture(&tp, plan, 24_000);
    let probes = r.controller.capture().unwrap().probes();
    assert_eq!(probes.len(), 3);
    let of = |p: PluginInstanceId| {
        let probe = probes.iter().find(|x| x.info.plugin == p).unwrap();
        (probe.info.clone(), probe.stats())
    };
    let (info, s) = of(louder);
    assert_eq!((info.position, info.next), (0, Some(late)));
    assert!(s.frames >= 24_000);
    assert!((s.out_energy / s.in_energy - 16.0).abs() < 0.5, "{s:?}");
    assert!(s.out_above > 20_000 && s.in_above == 0);
    assert!((s.out_peak - 2.0).abs() < 1e-3);
    assert!(
        s.out_flat > 0,
        "a constant is flat (the rule weighs the input's too)"
    );
    let (info, s) = of(late);
    assert_eq!((info.position, info.next), (1, Some(mute)));
    assert!(s.in_energy > 0.0);
    assert!(
        s.diff_energy.unwrap() < s.in_energy * 1e-9,
        "aligned: a pure delay changes nothing ({s:?})"
    );
    let (info, s) = of(mute);
    assert_eq!(info.next, None);
    assert!(s.in_energy > 0.0 && s.out_energy == 0.0);
}

#[test]
fn nothing_is_built_or_changed_without_a_plan() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "T", ChannelLayout::Stereo);
    let imp = tp.impulse(2, 2000);
    tp.clip(t, imp, MusicalTime::from_quarters(0.25), 2000);
    device(&mut tp, t, builtin::LATENCY_PROBE, &[(0, 40.0)]);
    let mut plain = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    let nodes = plain.controller.graph_stats().nodes;
    plain.play_from(0).unwrap();
    let a = plain.render(12_000);
    let plan = CapturePlan {
        tracks: vec![t],
        devices: true,
        block: 0,
    };
    let (got, r) = capture(&tp, plan, 12_000 - 40);
    assert_eq!(
        r.controller.graph_stats().nodes,
        nodes + 2,
        "a tap and a probe"
    );
    assert_eq!(got.latency, 40);
    // The master is the same render.
    assert_eq!(&a[0][40..], &got.master[0][..]);
}
