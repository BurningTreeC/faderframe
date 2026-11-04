//! Ambisonic fields through the engine: mono and stereo tracks encoded by
//! direction (and their lanes), beds encoded, a field decoded into a bed
//! and on a stereo device, sends following the panner into a field, and
//! the listener's head turning binaural listening without a rebuild.
#![allow(clippy::unwrap_used)]
mod common;
use common::{TestProject, approx};
use faderframe_audio_files::AudioData;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::ambisonics::{Orientation, direction, harmonics};
use faderframe_core::{AmbiOrder, ChannelLayout, SurroundFormat, SurroundPan, SurroundParam};
use faderframe_core::{TrackId, surround::StereoSource};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{AuxSend, OutputRouting, SendTap, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn render(tp: &TestProject, outputs: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut r = OfflineRenderer::new(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        outputs,
    )
    .unwrap();
    r.play_from(0).unwrap();
    r.render(frames)
}

fn dc(channels: &[f32], frames: usize) -> AudioData {
    AudioData::from_channels(SR, channels.iter().map(|v| vec![*v; frames]).collect())
}

/// A project whose master is `master`, with a track of `layout` playing
/// `source` into it.
fn into(master: ChannelLayout, layout: ChannelLayout, source: AudioData) -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let m = tp.master();
    tp.project.track_mut(m).unwrap().layout = master;
    let t = tp.track(TrackKind::Audio, "Source", layout);
    let frames = source.frames();
    let src = tp.source(source);
    tp.clip(t, src, MusicalTime::ZERO, frames as i64);
    (tp, t)
}

fn toward(azimuth: f32, elevation: f32) -> SurroundPan {
    SurroundPan {
        azimuth,
        elevation,
        ..SurroundPan::default()
    }
}

/// A mono track into an ambisonic master is the harmonics at its
/// direction on the master's channels (16 outputs: the field passes to
/// the device as it is).
#[test]
fn a_mono_track_is_encoded_at_its_direction() {
    for order in AmbiOrder::ALL {
        for (az, el) in [(0.0f32, 0.0f32), (110.0, 30.0), (-45.0, -20.0)] {
            let (mut tp, t) = into(
                ChannelLayout::Ambisonic(order),
                ChannelLayout::Mono,
                dc(&[0.5], SR as usize),
            );
            tp.project.track_mut(t).unwrap().surround = toward(az, el);
            let out = render(&tp, order.channels(), 4096);
            let want = harmonics(direction(az, el));
            for k in 0..order.channels() {
                assert!(
                    approx(out[k][4000], 0.5 * want[k], 1e-5),
                    "{order:?} {az}/{el} ch {k}: {} vs {}",
                    out[k][4000],
                    0.5 * want[k]
                );
            }
        }
    }
}

/// A stereo image and a 5.1 bed into a first-order field: each channel at
/// its own direction.
#[test]
fn stereo_images_and_beds_are_encoded_by_their_directions() {
    let field = ChannelLayout::Ambisonic(AmbiOrder::First);
    let (mut tp, t) = into(field, ChannelLayout::Stereo, dc(&[0.6, 0.0], SR as usize));
    tp.project.track_mut(t).unwrap().surround = SurroundPan {
        azimuth: -90.0,
        width: 1.0,
        stereo: StereoSource::Image,
        ..SurroundPan::default()
    };
    let out = render(&tp, 4, 4096);
    // Its left channel 30° left of −90°.
    let want = harmonics(direction(-60.0, 0.0));
    for k in 0..4 {
        assert!(approx(out[k][4000], 0.6 * want[k], 1e-5), "ch {k}");
    }
    let f = SurroundFormat::S51;
    let mut levels = [0.0f32; 6];
    levels[4] = 0.5; // Ls only
    let (tp, _) = into(field, ChannelLayout::Surround(f), dc(&levels, SR as usize));
    let out = render(&tp, 4, 4096);
    let ls = f.speakers()[4];
    let want = harmonics(direction(ls.azimuth, ls.elevation));
    for k in 0..4 {
        assert!(approx(out[k][4000], 0.5 * want[k], 1e-5), "Ls ch {k}");
    }
}

/// A third-order bus decoded into a 7.1.4 master (AllRAD): a source at a
/// speaker's direction plays loudest there, its power near the source's.
#[test]
fn a_field_decodes_into_a_bed() {
    let f = SurroundFormat::S714;
    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Surround(f);
    let bus = tp.track(
        TrackKind::Bus,
        "Field",
        ChannelLayout::Ambisonic(AmbiOrder::Third),
    );
    let t = tp.track(TrackKind::Audio, "Source", ChannelLayout::Mono);
    let src = tp.source(dc(&[0.5], SR as usize));
    tp.clip(t, src, MusicalTime::ZERO, SR as i64);
    tp.project.track_mut(t).unwrap().output = OutputRouting::Track { track: bus };
    for (i, s) in f.speakers().iter().enumerate() {
        if s.lfe {
            continue;
        }
        tp.project.track_mut(t).unwrap().surround = toward(s.azimuth, s.elevation);
        let out = render(&tp, f.channels(), 4096);
        let at: Vec<f32> = out.iter().map(|c| c[4000]).collect();
        let loudest = (0..at.len())
            .max_by(|a, b| at[*a].total_cmp(&at[*b]))
            .unwrap();
        assert_eq!(loudest, i, "{}: {at:?}", s.label);
        let power: f32 = at.iter().map(|v| v * v).sum();
        let db = 10.0 * (power / 0.25).log10();
        assert!(db.abs() < 3.0, "{}: {db:.2} dB", s.label);
        let lfe = f.speakers().iter().position(|s| s.lfe).unwrap();
        assert_eq!(at[lfe], 0.0);
    }
}

/// An ambisonic master on a stereo device is decoded on the way out (a
/// source on the left plays on the left); a field the device holds passes
/// as it is.
#[test]
fn an_ambisonic_master_is_decoded_for_a_stereo_device() {
    let (mut tp, t) = into(
        ChannelLayout::Ambisonic(AmbiOrder::Second),
        ChannelLayout::Mono,
        dc(&[0.5], SR as usize),
    );
    tp.project.track_mut(t).unwrap().surround = toward(80.0, 0.0);
    let out = render(&tp, 2, 4096);
    assert!(
        out[0][4000] > 2.0 * out[1][4000].abs(),
        "{} {}",
        out[0][4000],
        out[1][4000]
    );
    tp.project.track_mut(t).unwrap().surround = toward(0.0, 0.0);
    let out = render(&tp, 2, 4096);
    assert!(approx(out[0][4000], out[1][4000], 1e-4));
}

/// The direction's lanes move the encoded source on time; distance
/// lowers it.
#[test]
fn azimuth_and_distance_lanes_move_the_source() {
    let (mut tp, t) = into(
        ChannelLayout::Ambisonic(AmbiOrder::First),
        ChannelLayout::Mono,
        dc(&[0.5], SR as usize * 4),
    );
    for (target, points) in [
        (SurroundParam::Azimuth, [(0.0, 0.0), (1.0, 90.0)]),
        (SurroundParam::Distance, [(0.0, 1.0), (1.0, 2.0)]),
    ] {
        let id = tp.project.ids.allocate();
        tp.project
            .track_mut(t)
            .unwrap()
            .automation
            .lanes
            .push(AutomationLane {
                id,
                target: AutomationTarget::Surround(target),
                curve: AutomationCurve::from_points(
                    points
                        .iter()
                        .map(|(q, v)| AutomationPoint {
                            time: MusicalTime::from_quarters(*q),
                            value: *v,
                            shape: CurveShape::Linear,
                        })
                        .collect(),
                ),
                mode: AutomationMode::Read,
                visible: true,
            });
    }
    // After the lanes' end (a quarter at 120 bpm = 24 000 frames): 90°
    // left at half the level.
    let out = render(&tp, 4, 40_000);
    let want = harmonics(direction(90.0, 0.0));
    for k in 0..4 {
        assert!(
            approx(out[k][36_000], 0.25 * want[k], 1e-4),
            "ch {k}: {}",
            out[k][36_000]
        );
    }
}

/// A mono track's pre-fader send into an ambisonic aux follows the
/// track's panner (as into a bed).
#[test]
fn a_pre_fader_send_into_a_field_follows_the_panner() {
    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Ambisonic(AmbiOrder::First);
    let aux = tp.track(
        TrackKind::Aux,
        "Reverb",
        ChannelLayout::Ambisonic(AmbiOrder::First),
    );
    let t = tp.track(TrackKind::Audio, "Source", ChannelLayout::Mono);
    let src = tp.source(dc(&[0.5], SR as usize));
    tp.clip(t, src, MusicalTime::ZERO, SR as i64);
    {
        let track = tp.project.track_mut(t).unwrap();
        track.surround = toward(-90.0, 0.0);
        track.output = OutputRouting::None;
    }
    let id = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().sends.push(AuxSend {
        id,
        target: aux,
        level_db: 0.0,
        tap: SendTap::PreFader,
        enabled: true,
        pan: None,
    });
    let out = render(&tp, 4, 4096);
    let want = harmonics(direction(-90.0, 0.0));
    for k in 0..4 {
        assert!(
            approx(out[k][4000], 0.5 * want[k], 1e-4),
            "ch {k}: {}",
            out[k][4000]
        );
    }
}

/// On headphones, turning the head turns the scene — a bed's centre
/// speaker ends on the right ear when the head turns left, an ambisonic
/// field's source likewise — with no rebuild, and speakers never turn.
#[test]
fn turning_the_head_turns_binaural_listening() {
    let n = SR as usize * 2;
    // About 3 kHz: the head shadows it well.
    let tone: Vec<f32> = (0..n).map(|i| 0.4 * (i as f32 * 0.4).sin()).collect();
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    for master in [
        ChannelLayout::Surround(SurroundFormat::S51),
        ChannelLayout::Ambisonic(AmbiOrder::Third),
    ] {
        let (mut tp, t) = into(
            master,
            ChannelLayout::Mono,
            AudioData::from_channels(SR, vec![tone.clone()]),
        );
        tp.project.track_mut(t).unwrap().surround = SurroundPan {
            panner: faderframe_core::surround::SurroundPanner::Vbap,
            ..toward(0.0, 0.0)
        };
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2)
            .unwrap();
        r.play_from(0).unwrap();
        r.controller
            .set_binaural(Some(faderframe_binaural::Room::Near));
        r.controller.relisten(&tp.project);
        let ahead = r.render(9600);
        let (l, rr) = (rms(&ahead[0][4800..]), rms(&ahead[1][4800..]));
        assert!(
            (l - rr).abs() < 0.25 * l.max(rr),
            "{master:?} ahead: {l} {rr}"
        );
        r.controller
            .set_head_orientation(Orientation::from_ypr(90.0, 0.0, 0.0));
        assert!(
            r.controller
                .head_orientation()
                .angle_to(&Orientation::from_ypr(90.0, 0.0, 0.0))
                < 1e-3
        );
        let turned = r.render(19_200);
        let (l, rr) = (rms(&turned[0][9600..]), rms(&turned[1][9600..]));
        assert!(rr > 1.5 * l, "{master:?} turned: {l} {rr}");
        r.controller
            .sync(&tp.project, &tp.sources, faderframe_project::Impact::None)
            .unwrap();
    }
    // Speakers never turn: a 5.1 master on six outputs plays as before.
    let (mut tp, t) = into(
        ChannelLayout::Surround(SurroundFormat::S51),
        ChannelLayout::Mono,
        AudioData::from_channels(SR, vec![tone.clone()]),
    );
    tp.project.track_mut(t).unwrap().surround = toward(0.0, 0.0);
    let plain = render(&tp, 6, 4096);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    r.controller
        .set_head_orientation(Orientation::from_ypr(90.0, 0.0, 0.0));
    let out = r.render(4096);
    for c in 0..6 {
        for i in 0..4096 {
            assert_eq!(out[c][i], plain[c][i], "ch {c} at {i}");
        }
    }
}
