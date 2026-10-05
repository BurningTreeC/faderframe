#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{ChannelLayout, TrackId};
use faderframe_engine::{
    EngineConfig,
    offline::{OfflineRenderer, render_project},
};
use faderframe_project::{Command, History, OutputRouting, TrackKind};
use faderframe_timeline::MusicalTime;

fn rig(rate: u32, freq: f64, layout: ChannelLayout) -> (TestProject, [TrackId; 3]) {
    let mut tp = TestProject::new(rate);
    tp.project.crosstalk = true;
    let ids = std::array::from_fn(|i| tp.track(TrackKind::Audio, &format!("Track {i}"), layout));
    let signal: Vec<f32> = (0..rate as usize)
        .map(|n| (std::f64::consts::TAU * freq * n as f64 / rate as f64).sin() as f32 * 0.5)
        .collect();
    let source = tp.source(AudioData::from_channels(
        rate,
        vec![signal; layout.channel_count()],
    ));
    tp.clip(ids[0], source, MusicalTime::ZERO, i64::from(rate));
    // Listen only to the recipient. Disconnecting a donor's output does
    // not mute it; it still exists as a console channel.
    tp.project.track_mut(ids[0]).unwrap().output = OutputRouting::None;
    tp.project.track_mut(ids[2]).unwrap().output = OutputRouting::None;
    (tp, ids)
}

fn render(tp: &TestProject, block: usize) -> Vec<Vec<f32>> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig {
            sample_rate: tp.project.sample_rate,
            ..EngineConfig::default()
        },
        block,
        0,
        tp.project.sample_rate as usize / 4,
    )
    .unwrap()
}
fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}
fn level(out: &[Vec<f32>]) -> f64 {
    20.0 * (rms(&out[0][1024..]) / (0.5 / 2f64.sqrt())).log10()
}

#[test]
fn subtle_frequency_dependent_leakage_is_reciprocal_and_block_independent() {
    for rate in [44_100, 48_000, 96_000] {
        let (mut tp, ids) = rig(rate, 1000.0, ChannelLayout::Stereo);
        let a = render(&tp, 64);
        let db = level(&a);
        assert!((-86.0..-83.0).contains(&db), "{rate} Hz: {db} dB");
        let b = render(&tp, 512);
        assert_eq!(a, b);
        tp.project.tracks.swap(0, 1);
        assert_eq!(a, render(&tp, 64), "both directions use the same coupling");
        // Only the neighbour leaks: no recursive two-hop crossfeed.
        tp.project.track_mut(ids[1]).unwrap().output = OutputRouting::None;
        tp.project.track_mut(ids[2]).unwrap().output = OutputRouting::Master;
        tp.project.tracks.swap(0, 1);
        assert!(render(&tp, 64).iter().flatten().all(|&x| x == 0.0));
        let (high, _) = rig(rate, 10_000.0, ChannelLayout::Stereo);
        let high_db = level(&render(&high, 128));
        assert!(high_db > db + 15.0 && high_db < -65.0, "{db} → {high_db}");
    }
}

#[test]
fn off_mute_solo_and_bus_boundaries_isolate_tracks() {
    let (tp, ids) = rig(48_000, 1000.0, ChannelLayout::Stereo);
    for case in 0..5 {
        let mut p = TestProject {
            project: tp.project.clone(),
            sources: tp.sources.clone(),
        };
        match case {
            0 => p.project.crosstalk = false,
            1 => p.project.track_mut(ids[0]).unwrap().mute = true,
            2 => p.project.track_mut(ids[1]).unwrap().mute = true,
            3 => p.project.track_mut(ids[1]).unwrap().solo = true,
            _ => p.project.track_mut(ids[1]).unwrap().kind = TrackKind::Bus,
        }
        assert!(
            render(&p, 128).iter().flatten().all(|&x| x == 0.0),
            "case {case}"
        );
    }
    let (mut mono, ids) = rig(48_000, 1000.0, ChannelLayout::Mono);
    mono.project.track_mut(ids[1]).unwrap().layout = ChannelLayout::Stereo;
    let out = render(&mono, 128);
    assert!(level(&out) > -90.0);
    assert_eq!(out[0], out[1]);
}

#[test]
fn routing_cycles_are_skipped_and_reported_without_breaking_audio() {
    let (mut tp, ids) = rig(48_000, 1000.0, ChannelLayout::Stereo);
    tp.route(ids[0], ids[1]);
    let config = EngineConfig::default();
    let r = OfflineRenderer::new(&tp.project, &tp.sources, config, 128, 2).unwrap();
    assert!(
        r.controller
            .warnings()
            .iter()
            .any(|w| w.contains("Crosstalk skipped"))
    );
    let enabled = render(&tp, 128);
    tp.project.crosstalk = false;
    assert_eq!(enabled, render(&tp, 128));
}

#[test]
fn live_switch_reorder_and_undo_change_the_actual_neighbours() {
    let (mut tp, ids) = rig(48_000, 1000.0, ChannelLayout::Stereo);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 128, 2).unwrap();
    let mut h = History::default();
    r.play_from(0).unwrap();
    assert!(rms(&r.render(4096)[0]) > 1e-5);
    let impact = h
        .apply(
            &mut tp.project,
            Command::MoveTrack {
                track: ids[2],
                index: 1,
            },
        )
        .unwrap();
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let silent = r.render(4096);
    assert!(silent[0][1024..].iter().all(|&x| x == 0.0));
    let undo = h.undo(&mut tp.project).unwrap().unwrap();
    r.controller
        .sync(&tp.project, &tp.sources, undo.impact)
        .unwrap();
    assert!(rms(&r.render(4096)[0][1024..]) > 1e-5);
    let impact = h
        .apply(&mut tp.project, Command::SetCrosstalk { enabled: false })
        .unwrap();
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    assert!(r.render(4096)[0][1024..].iter().all(|&x| x == 0.0));
}

#[test]
fn mute_automation_on_a_donor_or_its_vca_silences_the_leak() {
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    for vca in [false, true] {
        let (mut tp, ids) = rig(48_000, 1000.0, ChannelLayout::Stereo);
        let target = if vca {
            let master = tp.track(TrackKind::Vca, "VCA", ChannelLayout::Mono);
            tp.project.track_mut(ids[0]).unwrap().vca = Some(master);
            master
        } else {
            ids[0]
        };
        let lane = AutomationLane {
            id: tp.project.ids.allocate(),
            target: AutomationTarget::TrackMute,
            curve: AutomationCurve::from_points(vec![
                AutomationPoint {
                    time: MusicalTime::ZERO,
                    value: 0.0,
                    shape: CurveShape::Step,
                },
                AutomationPoint {
                    time: MusicalTime::from_quarters(0.25),
                    value: 1.0,
                    shape: CurveShape::Step,
                },
            ]),
            mode: AutomationMode::Read,
            visible: true,
        };
        tp.project
            .track_mut(target)
            .unwrap()
            .automation
            .lanes
            .push(lane);
        let out = render(&tp, 128);
        assert!(rms(&out[0][1024..5000]) > 1e-5);
        assert!(out[0][7000..].iter().all(|&x| x == 0.0), "VCA: {vca}");
    }
}
