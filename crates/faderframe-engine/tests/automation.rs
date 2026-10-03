#![allow(clippy::unwrap_used)]
//! Automation through the engine: fader ramps, sample-accurate mute,
//! plugin parameters via events, latency-aligned soft bypass.

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{ChannelLayout, ParameterId, TrackId, builtin, db_to_gain};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::render_project;
use faderframe_project::{PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;

fn render(tp: &TestProject, frames: usize) -> Vec<f32> {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    render_project(&tp.project, &tp.sources, config, 256, 0, frames).unwrap()[0].clone()
}

fn lane(
    tp: &mut TestProject,
    track: TrackId,
    target: AutomationTarget,
    points: &[(f64, f64, CurveShape)],
) {
    let curve = AutomationCurve::from_points(
        points
            .iter()
            .map(|&(q, value, shape)| AutomationPoint {
                time: MusicalTime::from_quarters(q),
                value,
                shape,
            })
            .collect(),
    );
    let id = tp.project.ids.allocate();
    tp.project
        .track_mut(track)
        .unwrap()
        .automation
        .lanes
        .push(AutomationLane {
            id,
            target,
            curve,
            mode: AutomationMode::Read,
            visible: true,
        });
}

fn setup() -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, Q * 8);
    tp.clip(t, src, MusicalTime::ZERO, (Q * 8) as i64);
    (tp, t)
}

#[test]
fn volume_automation_follows_the_curve() {
    let (mut tp, t) = setup();
    let reference = render(&tp, 1000)[500];
    lane(
        &mut tp,
        t,
        AutomationTarget::TrackVolume,
        &[
            (0.0, 0.0, CurveShape::Linear),
            (2.0, -20.0, CurveShape::Step),
            (3.0, -6.0, CurveShape::Linear),
        ],
    );
    let out = render(&tp, Q * 4);
    let db = |i: usize| 20.0 * (out[i] / reference).log10();
    assert!(
        (db(Q) - -10.0).abs() < 0.05,
        "halfway down the ramp: {}",
        db(Q)
    );
    assert!(
        (db(Q * 2 + Q / 2) - -20.0).abs() < 0.05,
        "step holds -20 dB"
    );
    assert!(
        (db(Q * 3 + 100) - -6.0).abs() < 0.05,
        "jumps to -6 dB at beat 4"
    );
    // Smooth: no sample-to-sample jumps on the ramp.
    let max_step = out[100..Q * 2 - 100]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(max_step < 1e-4, "ramp step {max_step}");
}

#[test]
fn mute_automation_is_sample_accurate_and_independent_of_solo() {
    let (mut tp, t) = setup();
    lane(
        &mut tp,
        t,
        AutomationTarget::TrackMute,
        &[(0.0, 0.0, CurveShape::Step), (1.0, 1.0, CurveShape::Step)],
    );
    let out = render(&tp, Q * 2);
    assert!(out[Q - 40].abs() > 0.1, "audible before the mute point");
    assert!(out[Q + 40].abs() < 1e-6, "muted within one automation step");
    // Soloing the muted track does not unmute it.
    tp.project.track_mut(t).unwrap().solo = true;
    let out = render(&tp, Q * 2);
    assert!(out[Q + 40].abs() < 1e-6);
}

#[test]
fn plugin_parameters_are_automated_through_events() {
    let (mut tp, t) = setup();
    let plugin = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id: plugin,
        plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    let reference = render(&tp, 1000)[500];
    lane(
        &mut tp,
        t,
        AutomationTarget::PluginParameter {
            plugin,
            parameter: ParameterId(0),
        },
        &[(0.0, 0.0, CurveShape::Step), (1.0, -20.0, CurveShape::Step)],
    );
    let out = render(&tp, Q * 2);
    assert!((out[Q / 2] / reference - 1.0).abs() < 1e-3);
    assert!(
        (out[Q + Q / 2] / reference - db_to_gain(-20.0)).abs() < 1e-3,
        "{}",
        out[Q + Q / 2] / reference
    );
}

#[test]
fn automated_bypass_keeps_latency_alignment() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let ramp: Vec<f32> = (0..Q * 4).map(|i| (i % 1000) as f32 / 1000.0).collect();
    let src = tp.source(AudioData::from_channels(SR, vec![ramp.clone()]));
    tp.clip(t, src, MusicalTime::ZERO, (Q * 4) as i64);
    let plugin = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id: plugin,
        plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    let plain = render(&tp, Q * 3);
    lane(
        &mut tp,
        t,
        AutomationTarget::PluginBypass(plugin),
        &[(0.0, 0.0, CurveShape::Step), (2.0, 1.0, CurveShape::Step)],
    );
    let bypassed = render(&tp, Q * 3);
    // Before and after the bypass point the output is the same signal at
    // the same position (the dry path is delayed by the plugin latency).
    for i in [Q, Q * 2 + 2_000, Q * 2 + 20_000] {
        assert!(
            (plain[i] - bypassed[i]).abs() < 1e-5,
            "sample {i}: {} vs {}",
            plain[i],
            bypassed[i]
        );
    }
}

#[test]
fn vcas_scale_mute_and_solo_their_members_also_when_automated() {
    let (mut tp, t) = setup();
    let reference = render(&tp, 1000)[500];
    let db = |out: &[f32], i: usize| 20.0 * (out[i].abs() / reference).log10();
    let vca = tp.track(TrackKind::Vca, "VCA", ChannelLayout::Mono);
    let outer = tp.track(TrackKind::Vca, "Outer", ChannelLayout::Mono);
    tp.project.track_mut(t).unwrap().vca = Some(vca);
    tp.project.track_mut(vca).unwrap().vca = Some(outer);
    tp.project.track_mut(vca).unwrap().volume_db = -6.0;
    tp.project.track_mut(outer).unwrap().volume_db = -4.0;
    tp.project.track_mut(t).unwrap().volume_db = -2.0;
    let out = render(&tp, 1000);
    assert!((db(&out, 500) - -12.0).abs() < 0.05, "{}", db(&out, 500));

    // A muted (outer) VCA silences the member; a soloed one solos it.
    tp.project.track_mut(outer).unwrap().mute = true;
    assert!(render(&tp, 1000)[500].abs() < 1e-6);
    tp.project.track_mut(outer).unwrap().mute = false;
    let other = tp.track(TrackKind::Audio, "B", ChannelLayout::Mono);
    let src = tp.dc(1, 0.25, 1000);
    tp.clip(other, src, MusicalTime::ZERO, 1000);
    tp.project.track_mut(vca).unwrap().solo = true;
    assert!(
        (db(&render(&tp, 1000), 500) - -12.0).abs() < 0.05,
        "B is silenced"
    );
    tp.project.track_mut(vca).unwrap().solo = false;

    // Automating the inner VCA replaces its static -6 dB.
    lane(
        &mut tp,
        vca,
        AutomationTarget::TrackVolume,
        &[(0.0, 0.0, CurveShape::Step), (2.0, -20.0, CurveShape::Step)],
    );
    tp.project.track_mut(other).unwrap().mute = true;
    let out = render(&tp, Q * 3);
    assert!((db(&out, Q) - -6.0).abs() < 0.05, "{}", db(&out, Q));
    assert!((db(&out, Q * 2 + 500) - -26.0).abs() < 0.05);
    lane(
        &mut tp,
        outer,
        AutomationTarget::TrackMute,
        &[(0.0, 0.0, CurveShape::Step), (1.0, 1.0, CurveShape::Step)],
    );
    let out = render(&tp, Q * 2);
    assert!(out[Q / 2].abs() > 0.1 && out[Q + 500].abs() < 1e-6);
}
