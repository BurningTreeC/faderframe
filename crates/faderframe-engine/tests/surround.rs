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

/// An object joins the master after its strip, as a renderer adds it to
/// the bed: the master's fader leaves it alone (and turns down the bed),
/// it sends nothing to the LFE, and the master's meters show it.
#[test]
fn objects_skip_the_master_strip() {
    let mut tp = mono_in_51(SurroundPan {
        x: -1.0,
        y: -1.0,
        lfe_db: 0.0,
        ..SurroundPan::default()
    });
    let master = tp.master();
    tp.project.track_mut(master).unwrap().volume_db = -20.0;
    // A bed track at the centre beside it.
    let bed = tp.track(TrackKind::Audio, "Bed", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, 48_000);
    tp.clip(bed, src, MusicalTime::ZERO, 48_000);
    let object = tp
        .project
        .tracks
        .iter()
        .find(|t| t.name == "Mono")
        .unwrap()
        .id;
    let before = levels(&tp, 6);
    // In the bed: through the master fader, with the LFE send.
    let g = faderframe_core::db_to_gain(-20.0);
    assert!(close(before[4], 0.5 * g) && before[3] > 0.01, "{before:?}");
    tp.project.track_mut(object).unwrap().object = true;
    assert!(tp.project.is_object(tp.project.track(object).unwrap()));
    let after = levels(&tp, 6);
    assert!(close(after[4], 0.5), "the object at full level: {after:?}");
    assert!(after[3].abs() < 1e-6, "no LFE from an object: {after:?}");
    assert!(close(after[2], 0.5 * g), "the bed still through the fader");
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    r.render(2048);
    let m = r.controller.take_meter(master).unwrap();
    assert!(
        close(m.channels[4].peak, 0.5),
        "metered: {:?}",
        m.channels[4]
    );
    assert!(close(m.channels[2].peak, 0.5 * g));
}

/// A master back from 5.1 to stereo meters two channels again (its meter
/// range keeps its size, the count follows the format).
#[test]
fn meters_follow_the_format_back_to_stereo() {
    let mut tp = mono_in_51(SurroundPan::default());
    let master = tp.master();
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    r.render(1024);
    assert_eq!(r.controller.take_meter(master).unwrap().count, 6);
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Stereo;
    r.controller
        .sync(&tp.project, &tp.sources, faderframe_project::Impact::Graph)
        .unwrap();
    r.render(1024);
    let m = r.controller.take_meter(master).unwrap();
    assert_eq!(m.count, 2);
    let track = tp
        .project
        .tracks
        .iter()
        .find(|t| t.name == "Mono")
        .unwrap()
        .id;
    assert_eq!(r.controller.take_meter(track).unwrap().count, 2);
}

/// Sends meet beds through the panner: taken before the panner, a mono
/// track's send follows it into a 5.1 reverb (not every channel at full
/// level); a send after a 5.1 panner into a stereo reverb carries the bed
/// folded (not just its L and R).
#[test]
fn sends_follow_the_panner_into_beds_and_fold_out_of_them() {
    use faderframe_project::{AuxSend, SendTap};
    let send_to = |pan: SurroundPan, aux_layout: ChannelLayout, tap: SendTap| {
        let mut tp = mono_in_51(pan);
        let track = tp
            .project
            .tracks
            .iter()
            .find(|t| t.name == "Mono")
            .unwrap()
            .id;
        // The track itself silent at the master: only the send is heard.
        tp.project.track_mut(track).unwrap().output = OutputRouting::None;
        let aux = tp.track(TrackKind::Aux, "Verb", aux_layout);
        let id = tp.project.ids.allocate();
        tp.project.track_mut(track).unwrap().sends.push(AuxSend {
            id,
            target: aux,
            level_db: 0.0,
            tap,
            enabled: true,
        });
        levels(&tp, 6)
    };
    let back_left = SurroundPan {
        x: -1.0,
        y: -1.0,
        ..SurroundPan::default()
    };
    // Pre-fader into a 5.1 reverb: at Ls only.
    let pre = send_to(back_left, S51, SendTap::PreFader);
    assert!(close(pre[4], 0.5), "Ls: {pre:?}");
    for c in [0, 1, 2, 3, 5] {
        assert!(pre[c].abs() < 1e-6, "{c}: {pre:?}");
    }
    let pre_fx = send_to(back_left, S51, SendTap::PreFx);
    assert!(close(pre_fx[4], 0.5), "pre-FX Ls: {pre_fx:?}");
    // Panned to the centre of the 5.1 master, post-fader into a stereo
    // reverb: the centre folded into both sides at −3 dB (the reverb's
    // own meter, its output not connected).
    let mut tp = mono_in_51(SurroundPan::default());
    let track = tp
        .project
        .tracks
        .iter()
        .find(|t| t.name == "Mono")
        .unwrap()
        .id;
    let aux = tp.track(TrackKind::Aux, "Verb", ChannelLayout::Stereo);
    tp.project.track_mut(aux).unwrap().output = OutputRouting::None;
    let id = tp.project.ids.allocate();
    tp.project.track_mut(track).unwrap().sends.push(AuxSend {
        id,
        target: aux,
        level_db: 0.0,
        tap: SendTap::PostFader,
        enabled: true,
    });
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    r.render(2048);
    let m = r.controller.take_meter(aux).unwrap();
    let h = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!(
        close(m.channels[0].peak, h) && close(m.channels[1].peak, h),
        "{:?} {:?}",
        m.channels[0],
        m.channels[1]
    );
}
