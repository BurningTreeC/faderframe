//! Surround beds: tracks placed in a 5.1 master by the surround panner,
//! stereo spread by width, a 7.1.4 bus folded into it, LFE sends, and
//! meters for every channel.
#![allow(clippy::unwrap_used)]
mod common;
use common::TestProject;
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{OutputRouting, TrackKind};
use faderframe_timeline::MusicalTime;

const S51: ChannelLayout = ChannelLayout::Surround(SurroundFormat::S51);

/// The last frame of each of the master's six outputs after 4096 frames of
/// `tp`.
fn levels(tp: &TestProject, outputs: usize) -> Vec<f32> {
    let mut r = OfflineRenderer::new(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        outputs,
    )
    .unwrap();
    r.play_from(0).unwrap();
    let out = r.render(4096);
    out.iter().map(|c| *c.last().unwrap()).collect()
}

fn mono_in_51(pan: SurroundPan) -> TestProject {
    let mut tp = TestProject::new(48_000);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = S51;
    let t = tp.track(TrackKind::Audio, "Mono", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, 48_000);
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    tp.project.track_mut(t).unwrap().surround = pan;
    tp
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

#[test]
fn the_panner_places_a_mono_track_in_51() {
    // L R C LFE Ls Rs.
    let front = levels(&mono_in_51(SurroundPan::default()), 6);
    assert!(close(front[2], 0.5), "centre: {front:?}");
    assert!(
        front
            .iter()
            .enumerate()
            .all(|(i, v)| i == 2 || v.abs() < 1e-6)
    );
    let back_left = levels(
        &mono_in_51(SurroundPan {
            x: -1.0,
            y: -1.0,
            ..SurroundPan::default()
        }),
        6,
    );
    assert!(close(back_left[4], 0.5), "Ls: {back_left:?}");
    let between = levels(
        &mono_in_51(SurroundPan {
            x: -0.5,
            ..SurroundPan::default()
        }),
        6,
    );
    let h = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(between[0], h) && close(between[2], h), "{between:?}");
    let lfe = levels(
        &mono_in_51(SurroundPan {
            lfe_db: -6.0,
            ..SurroundPan::default()
        }),
        6,
    );
    assert!(
        close(lfe[3], 0.5 * faderframe_core::db_to_gain(-6.0)),
        "{lfe:?}"
    );
}

#[test]
fn stereo_spreads_by_width_and_a_714_bus_folds_into_51() {
    let mut tp = TestProject::new(48_000);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = S51;
    let t = tp.track(TrackKind::Audio, "Stereo", ChannelLayout::Stereo);
    let src = tp.source(faderframe_audio_files::AudioData::from_channels(
        48_000,
        vec![vec![0.3; 48_000], vec![0.6; 48_000]],
    ));
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    let l = levels(&tp, 6);
    assert!(close(l[0], 0.3) && close(l[1], 0.6), "{l:?}");
    // Narrower: both towards the centre.
    tp.project.track_mut(t).unwrap().surround.width = 0.0;
    let l = levels(&tp, 6);
    assert!(close(l[2], 0.9), "both on C: {l:?}");

    // A 7.1.4 bus fed by a mono track placed top-rear-left, into the 5.1
    // master: the height folds onto the floor (Ls).
    let mut tp = TestProject::new(48_000);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = S51;
    let bus = tp.track(
        TrackKind::Bus,
        "Atmos",
        ChannelLayout::Surround(SurroundFormat::S714),
    );
    let t = tp.track(TrackKind::Audio, "Fly", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, 48_000);
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    let tr = tp.project.track_mut(t).unwrap();
    tr.output = OutputRouting::Track { track: bus };
    tr.surround = SurroundPan {
        x: -1.0,
        y: -1.0,
        z: 1.0,
        ..SurroundPan::default()
    };
    let l = levels(&tp, 6);
    assert!(close(l[4], 0.5), "Ltr folds onto Ls: {l:?}");
    assert!(l.iter().enumerate().all(|(i, v)| i == 4 || v.abs() < 1e-6));
}

#[test]
fn a_bed_is_metered_on_every_channel() {
    let tp = mono_in_51(SurroundPan {
        x: 1.0,
        y: -1.0,
        ..SurroundPan::default()
    });
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    r.render(2048);
    let master = tp.project.master_id().unwrap();
    let m = r.controller.take_meter(master).unwrap();
    assert_eq!(m.count, 6);
    assert!(close(m.channels[5].peak, 0.5), "Rs: {:?}", m.channels[5]);
    assert!(m.channels[0].peak < 1e-6);
}

/// A 5.1 master on a stereo device is folded down: the surrounds to their
/// side, the centre into both at −3 dB; on six outputs nothing folds.
#[test]
fn a_bed_folds_down_to_a_stereo_device() {
    let rear = levels(
        &mono_in_51(SurroundPan {
            x: -1.0,
            y: -1.0,
            ..SurroundPan::default()
        }),
        2,
    );
    assert_eq!(rear.len(), 2);
    assert!(close(rear[0], 0.5) && rear[1].abs() < 1e-6, "{rear:?}");
    let centre = levels(&mono_in_51(SurroundPan::default()), 2);
    let h = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(centre[0], h) && close(centre[1], h), "{centre:?}");
    let six = levels(&mono_in_51(SurroundPan::default()), 6);
    assert!(close(six[2], 0.5));
}

/// An automation lane moves the track through the room: left to right
/// across the front while it plays, the strip following the curve.
#[test]
fn surround_automation_moves_the_track() {
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::SurroundParam;
    let q = 24_000usize;
    let mut tp = TestProject::new(48_000);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = S51;
    let t = tp.track(TrackKind::Audio, "Mono", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, q * 4);
    tp.clip(t, src, MusicalTime::ZERO, (q * 4) as i64);
    let point = |quarters: f64, value: f64| AutomationPoint {
        time: MusicalTime::from_quarters(quarters),
        value,
        shape: CurveShape::Linear,
    };
    let id = tp.project.ids.allocate();
    tp.project
        .track_mut(t)
        .unwrap()
        .automation
        .lanes
        .push(AutomationLane {
            id,
            target: AutomationTarget::Surround(SurroundParam::X),
            curve: AutomationCurve::from_points(vec![
                point(0.0, -1.0),
                point(1.0, -1.0),
                point(2.0, 1.0),
            ]),
            mode: AutomationMode::Read,
            visible: true,
        });
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    let out = r.render(q * 3);
    assert_eq!(out.len(), 6);
    let at = |c: usize, f: usize| out[c][f];
    // Hard left, then (halfway) centre, then hard right.
    assert!(close(at(0, q / 2), 0.5) && at(1, q / 2).abs() < 1e-6);
    assert!(
        (at(2, q + q / 2) - 0.5).abs() < 0.01,
        "{}",
        at(2, q + q / 2)
    );
    assert!(close(at(1, q * 5 / 2), 0.5) && at(0, q * 5 / 2).abs() < 1e-6);
}
