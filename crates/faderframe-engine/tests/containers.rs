//! Containers: parallel chains of devices, mixed, their latencies aligned.
#![allow(clippy::unwrap_used)]
mod common;
use common::TestProject;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, builtin, db_to_gain};
use faderframe_engine::{EngineConfig, offline::render_project};
use faderframe_project::container::Chain;
use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;

fn slot(tp: &mut TestProject, id: &str, set: &[(u32, f64)]) -> PluginSlot {
    PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: set
            .iter()
            .map(|(id, value)| SavedParameter {
                id: ParameterId(*id),
                value: *value,
            })
            .collect(),
        state: None,
        sidechain: None,
    }
}

fn render(tp: &TestProject, frames: usize) -> Vec<f32> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        0,
        frames,
    )
    .unwrap()
    .swap_remove(0)
}

/// A track of a constant 0.5 with a container: chain "Down" (a utility at
/// −6 dB) and chain "Dry".
fn setup() -> (TestProject, TrackId, PluginInstanceId) {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, 48_000);
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    let container = slot(&mut tp, builtin::CONTAINER, &[]);
    let down = slot(&mut tp, builtin::GAIN, &[(0, -6.0)]);
    let id = container.id;
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(container);
    let mut a = Chain::new("Down");
    a.inserts.push(down);
    track.containers.insert(id, vec![a, Chain::new("Dry")]);
    (tp, t, id)
}

#[test]
fn chains_are_mixed_and_muted_soloed_and_levelled() {
    let (mut tp, t, c) = setup();
    let both = 0.5 * (1.0 + db_to_gain(-6.0));
    let out = render(&tp, 8_000);
    assert!((out[6_000] - both).abs() < 1e-3, "{}", out[6_000]);
    fn chains(tp: &mut TestProject, t: TrackId, c: PluginInstanceId) -> &mut Vec<Chain> {
        tp.project
            .track_mut(t)
            .unwrap()
            .containers
            .get_mut(&c)
            .unwrap()
    }
    chains(&mut tp, t, c)[1].mute = true;
    let out = render(&tp, 8_000);
    assert!((out[6_000] - 0.5 * db_to_gain(-6.0)).abs() < 1e-3);
    chains(&mut tp, t, c)[1].mute = false;
    chains(&mut tp, t, c)[1].solo = true;
    let out = render(&tp, 8_000);
    assert!((out[6_000] - 0.5).abs() < 1e-3, "solo: the dry chain alone");
    chains(&mut tp, t, c)[1].solo = false;
    chains(&mut tp, t, c)[1].gain_db = -6.0;
    let out = render(&tp, 8_000);
    assert!(
        (out[6_000] - db_to_gain(-6.0)).abs() < 1e-3,
        "{}",
        out[6_000]
    );
    // Bypassed: the dry signal.
    tp.project.track_mut(t).unwrap().inserts[0].bypass = true;
    let out = render(&tp, 8_000);
    assert!((out[6_000] - 0.5).abs() < 1e-3);
}

#[test]
fn a_late_chain_is_aligned_with_the_others() {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "Click", ChannelLayout::Stereo);
    let src = tp.impulse(2, 4_096);
    tp.clip(t, src, MusicalTime::from_quarters(1.0), 4_096);
    let container = slot(&mut tp, builtin::CONTAINER, &[]);
    let late = slot(&mut tp, builtin::LATENCY_PROBE, &[(0, 300.0)]);
    let id = container.id;
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(container);
    let mut a = Chain::new("Late");
    a.inserts.push(late);
    track.containers.insert(id, vec![a, Chain::new("Dry")]);
    let out = render(&tp, 48_000);
    // One click of both chains, not two.
    let hits: Vec<(usize, f32)> = out
        .iter()
        .enumerate()
        .filter(|(_, v)| v.abs() > 1e-3)
        .map(|(i, v)| (i, *v))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!((hits[0].1 - 2.0).abs() < 1e-3, "{hits:?}");
}

#[test]
fn containers_nest() {
    let (mut tp, t, c) = setup();
    // In the dry chain: another container, both of whose chains are dry.
    let inner = slot(&mut tp, builtin::CONTAINER, &[]);
    let inner_id = inner.id;
    let track = tp.project.track_mut(t).unwrap();
    track.containers.get_mut(&c).unwrap()[1].inserts.push(inner);
    track
        .containers
        .insert(inner_id, vec![Chain::new("A"), Chain::new("B")]);
    let out = render(&tp, 8_000);
    let want = 0.5 * db_to_gain(-6.0) + 2.0 * 0.5;
    assert!((out[6_000] - want).abs() < 1e-3, "{}", out[6_000]);
    // The devices inside are the track's.
    let ids: Vec<_> = tp
        .project
        .track(t)
        .unwrap()
        .slots()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&inner_id));
}
