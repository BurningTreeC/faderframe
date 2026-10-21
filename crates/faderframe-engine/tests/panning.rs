#![allow(clippy::unwrap_used)]
//! Panning through the engine: mono pan by the project's law, stereo
//! balance, dual pan, their automation and smoothing, and how they meet
//! sends (their own pans too), buses, aux returns, beds and the master.

mod common;

use common::{TestProject, approx};
use faderframe_audio_files::AudioData;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{AudioSourceId, ChannelLayout, DualPan, PanLaw, PanMode, SendId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::{OfflineRenderer, render_project};
use faderframe_project::{AuxSend, Command, History, OutputRouting, SendTap, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;
/// A frame well inside the clips, past any start-up ramp.
const AT: usize = 3000;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

fn render(tp: &TestProject, block: usize, frames: usize) -> Vec<Vec<f32>> {
    render_project(&tp.project, &tp.sources, config(), block, 0, frames).unwrap()
}

/// A stereo source with its own constant value on each side.
fn stereo_dc(tp: &mut TestProject, left: f32, right: f32, frames: usize) -> AudioSourceId {
    tp.source(AudioData::from_channels(
        SR,
        vec![vec![left; frames], vec![right; frames]],
    ))
}

/// A track playing `source` from the start for `frames` frames.
fn track_with(
    tp: &mut TestProject,
    name: &str,
    layout: ChannelLayout,
    source: AudioSourceId,
    frames: usize,
) -> TrackId {
    let t = tp.track(TrackKind::Audio, name, layout);
    tp.clip(t, source, MusicalTime::ZERO, frames as i64);
    t
}

fn lane(tp: &mut TestProject, track: TrackId, target: AutomationTarget, points: &[(f64, f64)]) {
    let curve = AutomationCurve::from_points(
        points
            .iter()
            .map(|&(q, value)| AutomationPoint {
                time: MusicalTime::from_quarters(q),
                value,
                shape: CurveShape::Linear,
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

/// `(in_l, in_r)` through `m` (`m[source][destination]`).
fn through(m: [[f32; 2]; 2], l: f32, r: f32) -> (f32, f32) {
    (l * m[0][0] + r * m[1][0], l * m[0][1] + r * m[1][1])
}

#[test]
fn mono_pan_follows_the_projects_law() {
    for law in PanLaw::ALL {
        for pan in [-1.0f32, -0.75, 0.0, 0.75, 1.0] {
            let mut tp = TestProject::new(SR);
            let src = tp.dc(1, 0.5, 9600);
            let t = track_with(&mut tp, "A", ChannelLayout::Mono, src, 9600);
            tp.project.pan_law = law;
            tp.project.track_mut(t).unwrap().pan = pan;
            let out = render(&tp, 256, 4096);
            let (l, r) = law.mono_gains(pan);
            assert!(
                approx(out[0][AT], 0.5 * l, 1e-6),
                "{law:?} {pan}: {}",
                out[0][AT]
            );
            assert!(
                approx(out[1][AT], 0.5 * r, 1e-6),
                "{law:?} {pan}: {}",
                out[1][AT]
            );
        }
    }
    // The default law: −3.0103 dB on both sides at centre, exact sides.
    let mut tp = TestProject::new(SR);
    let src = tp.dc(1, 1.0, 9600);
    let t = track_with(&mut tp, "A", ChannelLayout::Mono, src, 9600);
    let out = render(&tp, 256, 4096);
    assert!(approx(20.0 * out[0][AT].log10(), -3.0103, 1e-3));
    tp.project.track_mut(t).unwrap().pan = 1.0;
    let out = render(&tp, 256, 4096);
    assert_eq!((out[0][AT], out[1][AT]), (0.0, 1.0));
}

#[test]
fn balance_keeps_left_left_and_right_right() {
    for pan in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
        // Unequal sides, opposite polarity: nothing crosses over.
        let mut tp = TestProject::new(SR);
        let src = stereo_dc(&mut tp, 0.8, -0.2, 9600);
        let t = track_with(&mut tp, "A", ChannelLayout::Stereo, src, 9600);
        tp.project.track_mut(t).unwrap().pan = pan;
        let out = render(&tp, 256, 4096);
        let (gl, gr) = faderframe_core::pan::stereo_balance(pan);
        assert_eq!(out[0][AT], 0.8 * gl, "{pan}");
        assert_eq!(out[1][AT], -0.2 * gr, "{pan}");
        // Only the left side playing: the right output stays silent.
        let mut tp = TestProject::new(SR);
        let src = stereo_dc(&mut tp, 0.8, 0.0, 9600);
        let t = track_with(&mut tp, "A", ChannelLayout::Stereo, src, 9600);
        tp.project.track_mut(t).unwrap().pan = pan;
        let out = render(&tp, 256, 4096);
        assert!(out[1].iter().all(|&x| x == 0.0), "{pan}: crossfeed");
    }
}

#[test]
fn dual_pan_places_each_channel() {
    let law = PanLaw::ConstantPower3dB;
    for pan in [
        DualPan::ORIGINAL,
        DualPan {
            left: -0.5,
            right: 0.5,
        },
        DualPan {
            left: 0.2,
            right: 0.9,
        },
        DualPan {
            left: 0.0,
            right: 0.0,
        },
        DualPan {
            left: 1.0,
            right: -1.0,
        },
        DualPan {
            left: -1.0,
            right: -0.25,
        },
    ] {
        let mut tp = TestProject::new(SR);
        let src = stereo_dc(&mut tp, 0.6, -0.2, 9600);
        let t = track_with(&mut tp, "A", ChannelLayout::Stereo, src, 9600);
        let tr = tp.project.track_mut(t).unwrap();
        tr.pan_mode = PanMode::Dual;
        tr.dual_pan = pan;
        let out = render(&tp, 256, 4096);
        let (l, r) = through(faderframe_core::pan::dual_pan(law, pan), 0.6, -0.2);
        assert!(
            approx(out[0][AT], l, 1e-6),
            "{pan:?}: {} vs {l}",
            out[0][AT]
        );
        assert!(
            approx(out[1][AT], r, 1e-6),
            "{pan:?}: {} vs {r}",
            out[1][AT]
        );
    }
}

#[test]
fn every_mode_at_rest_is_the_source_as_it_is() {
    // Balance at centre and dual pan at its defaults: bit for bit the
    // source (stereo in, stereo out; no gain change between the modes).
    let mut tp = TestProject::new(SR);
    let src = stereo_dc(&mut tp, 0.3, -0.7, 9600);
    let t = track_with(&mut tp, "A", ChannelLayout::Stereo, src, 9600);
    let balanced = render(&tp, 256, 4096);
    tp.project.track_mut(t).unwrap().pan_mode = PanMode::Dual;
    let dual = render(&tp, 256, 4096);
    assert_eq!(balanced, dual);
    assert_eq!((dual[0][AT], dual[1][AT]), (0.3, -0.7));
}

#[test]
fn the_pan_law_never_touches_balance() {
    let mut tp = TestProject::new(SR);
    let src = stereo_dc(&mut tp, 0.5, 0.5, 9600);
    let t = track_with(&mut tp, "A", ChannelLayout::Stereo, src, 9600);
    tp.project.track_mut(t).unwrap().pan = 0.4;
    let reference = render(&tp, 256, 4096);
    for law in PanLaw::ALL {
        tp.project.pan_law = law;
        assert_eq!(render(&tp, 256, 4096), reference, "{law:?}");
    }
}

#[test]
fn silence_stays_silent_and_bad_values_play_as_centre() {
    let mut tp = TestProject::new(SR);
    let quiet = tp.dc(2, 0.0, 9600);
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, quiet, 9600);
    let tr = tp.project.track_mut(s).unwrap();
    tr.pan_mode = PanMode::Dual;
    tr.dual_pan = DualPan {
        left: 0.3,
        right: -0.6,
    };
    let quiet = tp.dc(1, 0.0, 9600);
    let m = track_with(&mut tp, "M", ChannelLayout::Mono, quiet, 9600);
    tp.project.track_mut(m).unwrap().pan = -0.4;
    let out = render(&tp, 256, 4096);
    assert!(out.iter().flatten().all(|&x| x == 0.0));

    // Values that should never be there (a hand-edited file) neither
    // poison the audio nor pan anywhere surprising.
    let mut tp = TestProject::new(SR);
    let src = tp.dc(1, 1.0, 9600);
    let m = track_with(&mut tp, "M", ChannelLayout::Mono, src, 9600);
    tp.project.track_mut(m).unwrap().pan = f32::NAN;
    let src = stereo_dc(&mut tp, 0.25, 0.5, 9600);
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, src, 9600);
    let tr = tp.project.track_mut(s).unwrap();
    tr.pan_mode = PanMode::Dual;
    tr.dual_pan = DualPan {
        left: f32::NAN,
        right: f32::INFINITY,
    };
    let out = render(&tp, 256, 4096);
    assert!(out.iter().flatten().all(|x| x.is_finite()));
    let c = std::f32::consts::FRAC_1_SQRT_2;
    assert!(approx(out[0][AT], c + 0.25, 1e-6), "{}", out[0][AT]);
    assert!(approx(out[1][AT], c + 0.5, 1e-6), "{}", out[1][AT]);
}

#[test]
fn a_pan_jump_follows_the_law_on_its_way() {
    // Hard left to hard right in one move: the strip glides the position
    // across the block, so the power stays (nearly) constant instead of
    // dipping 3 dB halfway as a crossfade of the gains would.
    for block in [64usize, 512, 1024] {
        let mut tp = TestProject::new(SR);
        let src = tp.dc(1, 1.0, SR as usize);
        let t = track_with(&mut tp, "A", ChannelLayout::Mono, src, SR as usize);
        tp.project.track_mut(t).unwrap().pan = -1.0;
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), block, 2).unwrap();
        r.play_from(0).unwrap();
        let before = r.render(4096);
        assert_eq!((before[0][4095], before[1][4095]), (1.0, 0.0));
        let mut h = History::default();
        let impact = h
            .apply(&mut tp.project, Command::SetTrackPan { track: t, pan: 1.0 })
            .unwrap();
        r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
        let out = r.render(block * 2);
        for i in 0..block {
            let (l, rr) = (out[0][i], out[1][i]);
            let power = l * l + rr * rr;
            assert!(power > 0.999 && power < 1.000_01, "{block} @{i}: {power}");
            if i > 0 {
                assert!(l <= out[0][i - 1] + 1e-7 && rr >= out[1][i - 1] - 1e-7);
            }
        }
        // There at the end of the block, and staying there.
        assert!(approx(out[0][block - 1], 0.0, 1e-6) && approx(out[1][block - 1], 1.0, 1e-6));
        assert!(out[0][block..].iter().all(|&x| x == 0.0));
        assert!(out[1][block..].iter().all(|&x| x == 1.0));
    }
}

#[test]
fn switching_modes_and_laws_ramps_and_keeps_both_settings() {
    let mut tp = TestProject::new(SR);
    let src = stereo_dc(&mut tp, 0.5, 0.5, SR as usize);
    let t = track_with(&mut tp, "A", ChannelLayout::Stereo, src, SR as usize);
    let tr = tp.project.track_mut(t).unwrap();
    tr.pan = 1.0; // balance hard right
    tr.dual_pan = DualPan {
        left: -1.0,
        right: -1.0,
    }; // both channels hard left
    let block = 256;
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), block, 2).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(2048);
    assert_eq!((before[0][2047], before[1][2047]), (0.0, 0.5));

    let mut h = History::default();
    let impact = h
        .apply(
            &mut tp.project,
            Command::SetTrackPanMode {
                track: t,
                mode: PanMode::Dual,
            },
        )
        .unwrap();
    assert_eq!(impact, faderframe_project::Impact::Params);
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let out = r.render(block * 2);
    // A ramp, no step: neighbouring samples stay close.
    for (c, side) in out.iter().enumerate() {
        let worst = side[..block]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2.0 / block as f32, "{c}: {worst}");
    }
    assert_eq!((out[0][block * 2 - 1], out[1][block * 2 - 1]), (1.0, 0.0));
    // The balance setting waited: undo brings it back as it was.
    let impact = h.undo(&mut tp.project).unwrap().unwrap().impact;
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let back = r.render(block * 2);
    assert_eq!((back[0][block * 2 - 1], back[1][block * 2 - 1]), (0.0, 0.5));

    // A mono track: a new law takes effect without a rebuild, ramped.
    let mut tp = TestProject::new(SR);
    let src = tp.dc(1, 1.0, SR as usize);
    let m = track_with(&mut tp, "M", ChannelLayout::Mono, src, SR as usize);
    let _ = m;
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), block, 2).unwrap();
    r.play_from(0).unwrap();
    let _ = r.render(2048);
    let impact = h
        .apply(
            &mut tp.project,
            Command::SetPanLaw {
                law: PanLaw::Linear6dB,
            },
        )
        .unwrap();
    assert_eq!(impact, faderframe_project::Impact::Params);
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let out = r.render(block * 2);
    let c = std::f32::consts::FRAC_1_SQRT_2;
    assert!(out[0][0] < c && out[0][0] > 0.5 + (c - 0.5) * 0.9);
    assert_eq!(out[0][block * 2 - 1], 0.5);
}

#[test]
fn dual_pan_automation_moves_each_channel_on_time() {
    let law = PanLaw::ConstantPower3dB;
    let mut tp = TestProject::new(SR);
    let src = stereo_dc(&mut tp, 0.5, 0.0, Q * 4);
    let a = track_with(&mut tp, "A", ChannelLayout::Stereo, src, Q * 4);
    let src = stereo_dc(&mut tp, 0.0, 0.5, Q * 4);
    let b = track_with(&mut tp, "B", ChannelLayout::Stereo, src, Q * 4);
    for t in [a, b] {
        tp.project.track_mut(t).unwrap().pan_mode = PanMode::Dual;
    }
    // A's left channel sweeps left to right, B's right channel the other
    // way; B's left lane holds a value it never hears (its left is silent).
    lane(
        &mut tp,
        a,
        AutomationTarget::DualPanLeft,
        &[(0.0, -1.0), (1.0, 1.0)],
    );
    lane(
        &mut tp,
        b,
        AutomationTarget::DualPanRight,
        &[(0.0, 1.0), (1.0, -1.0)],
    );
    lane(&mut tp, b, AutomationTarget::DualPanLeft, &[(0.0, 0.3)]);
    // A balance lane does nothing while the track is dual-panned.
    lane(&mut tp, a, AutomationTarget::TrackPan, &[(0.0, 1.0)]);
    let out = render(&tp, 256, Q + 2048);
    let sweep = |f: usize| (-1.0 + 2.0 * f as f64 / Q as f64).min(1.0) as f32;
    for f in [AT, Q / 4, Q / 2, 3 * Q / 4, Q - 32, Q + 1000] {
        // The lane's value one frame on: what the strip reached there.
        let (al, ar) = law.mono_gains(sweep(f + 1));
        let (bl, br) = law.mono_gains(-sweep(f + 1));
        let (l, r) = (0.5 * (al + bl), 0.5 * (ar + br));
        assert!(approx(out[0][f], l, 2e-4), "@{f}: {} vs {l}", out[0][f]);
        assert!(approx(out[1][f], r, 2e-4), "@{f}: {} vs {r}", out[1][f]);
    }
    // Halfway: both at centre, each spread at −3 dB.
    let c = std::f32::consts::FRAC_1_SQRT_2;
    assert!(approx(out[0][Q / 2 - 1], c, 1e-3) && approx(out[1][Q / 2 - 1], c, 1e-3));
}

#[test]
fn automation_plays_the_same_whatever_the_block_size() {
    let mut tp = TestProject::new(SR);
    let src = stereo_dc(&mut tp, 0.4, -0.3, Q * 2);
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, src, Q * 2);
    tp.project.track_mut(s).unwrap().pan_mode = PanMode::Dual;
    lane(
        &mut tp,
        s,
        AutomationTarget::DualPanLeft,
        &[(0.0, -1.0), (0.3, 0.5), (0.31, -0.5), (1.0, 1.0)],
    );
    lane(
        &mut tp,
        s,
        AutomationTarget::DualPanRight,
        &[(0.0, 1.0), (0.7, -1.0)],
    );
    let src = tp.dc(1, 0.5, Q * 2);
    let m = track_with(&mut tp, "M", ChannelLayout::Mono, src, Q * 2);
    lane(
        &mut tp,
        m,
        AutomationTarget::TrackPan,
        &[(0.0, 1.0), (0.5, -1.0), (1.0, 0.25)],
    );
    let reference = render(&tp, 64, Q + 4096);
    for block in [32usize, 128, 256, 512] {
        assert_eq!(render(&tp, block, Q + 4096), reference, "block {block}");
    }
    // A jump within a few frames (the steep segment) stays a short ramp:
    // no sample moves further than the curve's own steepness allows.
    let worst = reference[0][..Q]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 0.05, "{worst}");
}

#[test]
fn tracks_and_buses_sum_with_each_pan_applied_once() {
    let law = PanLaw::ConstantPower3dB;
    let mut tp = TestProject::new(SR);
    let outer = tp.track(TrackKind::Bus, "Outer", ChannelLayout::Stereo);
    let inner = tp.track(TrackKind::Bus, "Inner", ChannelLayout::Stereo);
    let src = tp.dc(1, 0.5, 9600);
    let mono = track_with(&mut tp, "Mono", ChannelLayout::Mono, src, 9600);
    let src = stereo_dc(&mut tp, 0.2, 0.4, 9600);
    let stereo = track_with(&mut tp, "Stereo", ChannelLayout::Stereo, src, 9600);
    tp.route(mono, inner);
    tp.route(stereo, inner);
    tp.route(inner, outer);
    let p = &mut tp.project;
    let t = p.track_mut(mono).unwrap();
    t.pan = -0.3;
    t.volume_db = -6.020_6; // × 0.5: fader and pan compose
    let t = p.track_mut(stereo).unwrap();
    t.pan_mode = PanMode::Dual;
    t.dual_pan = DualPan {
        left: -0.2,
        right: 0.6,
    };
    p.track_mut(inner).unwrap().pan = 0.5; // balance
    let t = p.track_mut(outer).unwrap();
    t.pan_mode = PanMode::Dual;
    t.dual_pan = DualPan {
        left: -0.5,
        right: 0.5,
    };
    let out = render(&tp, 128, 4096);
    let (ml, mr) = law.mono_gains(-0.3);
    let (sl, sr) = through(
        faderframe_core::pan::dual_pan(
            law,
            DualPan {
                left: -0.2,
                right: 0.6,
            },
        ),
        0.2,
        0.4,
    );
    let (il, ir) = (0.5 * 0.5 * ml + sl, 0.5 * 0.5 * mr + sr);
    let (il, ir) = (il * 0.5, ir); // inner's balance at R50
    let (l, r) = through(
        faderframe_core::pan::dual_pan(
            law,
            DualPan {
                left: -0.5,
                right: 0.5,
            },
        ),
        il,
        ir,
    );
    // The master (balance at centre) adds nothing.
    assert!(approx(out[0][AT], l, 1e-5), "{} vs {l}", out[0][AT]);
    assert!(approx(out[1][AT], r, 1e-5), "{} vs {r}", out[1][AT]);
}

#[test]
fn into_a_mono_destination_there_is_nothing_to_pan() {
    let mut tp = TestProject::new(SR);
    let bus = tp.track(TrackKind::Bus, "Mono Bus", ChannelLayout::Mono);
    let src = stereo_dc(&mut tp, 0.8, 0.2, 9600);
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, src, 9600);
    tp.route(s, bus);
    let c = std::f32::consts::FRAC_1_SQRT_2;
    for (mode, pan) in [(PanMode::Balance, -1.0), (PanMode::Dual, 0.7)] {
        let t = tp.project.track_mut(s).unwrap();
        t.pan_mode = mode;
        t.pan = pan;
        t.dual_pan = DualPan {
            left: pan,
            right: pan,
        };
        let out = render(&tp, 256, 4096);
        // (L + R) / 2 into the bus, the bus panned to the centre.
        assert!(
            approx(out[0][AT], 0.5 * c, 1e-6),
            "{mode:?}: {}",
            out[0][AT]
        );
        assert!(approx(out[1][AT], 0.5 * c, 1e-6));
        assert!(!tp.project.pans_stereo(tp.project.track(s).unwrap()));
    }
}

#[test]
fn pre_fader_sends_are_unpanned_and_post_fader_sends_follow_the_pan() {
    let mut tp = TestProject::new(SR);
    let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Stereo);
    let src = tp.dc(1, 0.5, 9600);
    let a = track_with(&mut tp, "A", ChannelLayout::Mono, src, 9600);
    let send = AuxSend {
        id: SendId(tp.project.ids.allocate::<SendId>().raw()),
        target: aux,
        level_db: 0.0,
        tap: SendTap::PostFader,
        enabled: true,
        pan: None,
    };
    let t = tp.project.track_mut(a).unwrap();
    t.sends.push(send);
    t.pan = -1.0;
    // Post-fader: the panned signal, hard left — twice on the left.
    let out = render(&tp, 128, 4096);
    assert!(approx(out[0][AT], 1.0, 1e-6) && approx(out[1][AT], 0.0, 1e-6));
    // Pre-fader: before the fader and the pan; the mono signal reaches
    // both sides of the stereo aux at unity (the graph's channel rule).
    let t = tp.project.track_mut(a).unwrap();
    t.sends[0].tap = SendTap::PreFader;
    t.volume_db = faderframe_core::gain::SILENCE_DB;
    let out = render(&tp, 128, 4096);
    assert!(approx(out[0][AT], 0.5, 1e-5) && approx(out[1][AT], 0.5, 1e-5));
    // Dual pan on a stereo track: pre-fader sends keep the source's sides,
    // post-fader ones get the dual-panned image.
    let mut tp = TestProject::new(SR);
    let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Stereo);
    let src = stereo_dc(&mut tp, 0.6, 0.0, 9600);
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, src, 9600);
    let send = AuxSend {
        id: SendId(tp.project.ids.allocate::<SendId>().raw()),
        target: aux,
        level_db: 0.0,
        tap: SendTap::PreFader,
        enabled: true,
        pan: None,
    };
    let t = tp.project.track_mut(s).unwrap();
    t.sends.push(send);
    t.pan_mode = PanMode::Dual;
    t.dual_pan = DualPan {
        left: 1.0,
        right: -1.0,
    }; // swapped
    t.volume_db = faderframe_core::gain::SILENCE_DB;
    let out = render(&tp, 128, 4096);
    assert!(approx(out[0][AT], 0.6, 1e-5) && approx(out[1][AT], 0.0, 1e-5));
    let t = tp.project.track_mut(s).unwrap();
    t.sends[0].tap = SendTap::PostFader;
    t.volume_db = 0.0;
    let out = render(&tp, 128, 4096);
    assert!(approx(out[0][AT], 0.0, 1e-6) && approx(out[1][AT], 1.2, 1e-6));
}

#[test]
fn the_master_adds_no_pan_of_its_own() {
    let law = PanLaw::ConstantPower3dB;
    let mut tp = TestProject::new(SR);
    let src = tp.dc(1, 0.5, 9600);
    let a = track_with(&mut tp, "A", ChannelLayout::Mono, src, 9600);
    tp.project.track_mut(a).unwrap().pan = 0.3;
    let (l, r) = law.mono_gains(0.3);
    let master = tp.master();
    for mode in [PanMode::Balance, PanMode::Dual] {
        tp.project.track_mut(master).unwrap().pan_mode = mode;
        let out = render(&tp, 256, 4096);
        assert_eq!((out[0][AT], out[1][AT]), (0.5 * l, 0.5 * r), "{mode:?}");
    }
    // Its own balance applies once.
    tp.project.track_mut(master).unwrap().pan_mode = PanMode::Balance;
    tp.project.track_mut(master).unwrap().pan = -0.5;
    let out = render(&tp, 256, 4096);
    assert_eq!((out[0][AT], out[1][AT]), (0.5 * l, 0.5 * r * 0.5));
}

#[test]
fn a_sine_keeps_its_phase_through_every_mode() {
    // A sine on the left, the same sine inverted on the right: balance and
    // dual pan scale and mix them, never delay them (no latency).
    let n = 9600;
    let sine: Vec<f32> = (0..n)
        .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / SR as f32).sin() * 0.5)
        .collect();
    let inverted: Vec<f32> = sine.iter().map(|x| -x).collect();
    let mut tp = TestProject::new(SR);
    let src = tp.source(AudioData::from_channels(SR, vec![sine.clone(), inverted]));
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, src, n);
    let t = tp.project.track_mut(s).unwrap();
    t.pan_mode = PanMode::Dual;
    t.dual_pan = DualPan {
        left: 0.0,
        right: 0.0,
    };
    // Both channels centred: the antiphase pair cancels, as it would on
    // any mono fold — the source's doing, not the panner's.
    let out = render(&tp, 256, 4096);
    assert!(out[0][AT..].iter().all(|x| x.abs() < 1e-6));
    let t = tp.project.track_mut(s).unwrap();
    t.dual_pan = DualPan {
        left: -0.5,
        right: 0.5,
    };
    let out = render(&tp, 256, 4096);
    let m = faderframe_core::pan::dual_pan(PanLaw::default(), t_dual(-0.5, 0.5));
    for i in AT..AT + 200 {
        let (l, r) = through(m, sine[i], -sine[i]);
        assert!(
            approx(out[0][i], l, 1e-6) && approx(out[1][i], r, 1e-6),
            "@{i}"
        );
    }
}

fn t_dual(left: f32, right: f32) -> DualPan {
    DualPan { left, right }
}

/// A send from `from` to `to` (its own pan `pan`).
fn send_to(
    tp: &mut TestProject,
    from: TrackId,
    to: TrackId,
    tap: SendTap,
    pan: Option<f32>,
) -> SendId {
    let id = SendId(tp.project.ids.allocate::<SendId>().raw());
    tp.project.track_mut(from).unwrap().sends.push(AuxSend {
        id,
        target: to,
        level_db: 0.0,
        tap,
        enabled: true,
        pan,
    });
    id
}

/// Aux returns pan as every strip does, whatever feeds them: a stereo
/// return by its balance, a mono one by the pan law; fed by a mono or a
/// stereo track, from every tap. Only the return is heard (the track's
/// output is off: its taps carry the source as it is).
#[test]
fn aux_returns_pan_whatever_feeds_them() {
    let law = PanLaw::ConstantPower3dB;
    for aux_layout in [ChannelLayout::Stereo, ChannelLayout::Mono] {
        for (src_l, src_r, stereo) in [(0.5f32, 0.5, false), (0.6, -0.2, true)] {
            for tap in [SendTap::PreFx, SendTap::PreFader, SendTap::PostFader] {
                for pan in [-1.0f32, -0.4, 0.0, 0.7, 1.0] {
                    let mut tp = TestProject::new(SR);
                    let aux = tp.track(TrackKind::Aux, "Aux", aux_layout);
                    let (src, layout) = if stereo {
                        (
                            stereo_dc(&mut tp, src_l, src_r, 9600),
                            ChannelLayout::Stereo,
                        )
                    } else {
                        (tp.dc(1, src_l, 9600), ChannelLayout::Mono)
                    };
                    let a = track_with(&mut tp, "A", layout, src, 9600);
                    tp.project.track_mut(a).unwrap().output = OutputRouting::None;
                    send_to(&mut tp, a, aux, tap, None);
                    tp.project.track_mut(aux).unwrap().pan = pan;
                    let out = render(&tp, 256, 4096);
                    let (l, r) = if aux_layout == ChannelLayout::Stereo {
                        let (gl, gr) = faderframe_core::pan::stereo_balance(pan);
                        (src_l * gl, src_r * gr)
                    } else {
                        // Into a mono return (L + R) / 2, placed by the law.
                        let m = (src_l + src_r) * 0.5;
                        let (gl, gr) = law.mono_gains(pan);
                        (m * gl, m * gr)
                    };
                    let what = format!("{aux_layout:?} return, {layout:?} {tap:?}, pan {pan}");
                    assert!(
                        approx(out[0][AT], l, 1e-5),
                        "{what}: L {} vs {l}",
                        out[0][AT]
                    );
                    assert!(
                        approx(out[1][AT], r, 1e-5),
                        "{what}: R {} vs {r}",
                        out[1][AT]
                    );
                }
            }
        }
    }
}

/// A return's pan lane moves it (and its knob's value waits under it).
#[test]
fn an_aux_returns_pan_lane_moves_it() {
    let law = PanLaw::ConstantPower3dB;
    let mut tp = TestProject::new(SR);
    let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, Q * 2);
    let a = track_with(&mut tp, "A", ChannelLayout::Mono, src, Q * 2);
    tp.project.track_mut(a).unwrap().output = OutputRouting::None;
    send_to(&mut tp, a, aux, SendTap::PostFader, None);
    tp.project.track_mut(aux).unwrap().pan = 1.0;
    lane(
        &mut tp,
        aux,
        AutomationTarget::TrackPan,
        &[(0.0, -1.0), (1.0, 1.0)],
    );
    let out = render(&tp, 256, Q + 2048);
    let sweep = |f: usize| (-1.0 + 2.0 * f as f64 / Q as f64).min(1.0) as f32;
    for f in [AT, Q / 4, Q / 2, 3 * Q / 4, Q + 1000] {
        let (l, r) = law.mono_gains(sweep(f + 1));
        assert!(
            approx(out[0][f], 0.5 * l, 2e-4),
            "@{f}: {} vs {}",
            out[0][f],
            0.5 * l
        );
        assert!(
            approx(out[1][f], 0.5 * r, 2e-4),
            "@{f}: {} vs {}",
            out[1][f],
            0.5 * r
        );
    }
}

/// Into a surround bed a return follows its own spatial model: a mono
/// one the room panner (left: only the left speakers), a stereo one
/// balanced into the bed by its knob when it is set to.
#[test]
fn aux_returns_pan_into_a_surround_bed() {
    use faderframe_core::SurroundFormat;
    use faderframe_core::surround::StereoSource;
    let bed = ChannelLayout::Surround(SurroundFormat::S51);
    // L R C LFE Ls Rs.
    let right_side = [1usize, 5];
    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = bed;
    let mono = tp.track(TrackKind::Aux, "Mono Return", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, 9600);
    let a = track_with(&mut tp, "A", ChannelLayout::Mono, src, 9600);
    tp.project.track_mut(a).unwrap().output = OutputRouting::None;
    send_to(&mut tp, a, mono, SendTap::PreFader, None);
    tp.project.track_mut(mono).unwrap().surround.x = -1.0;
    let r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 6);
    let mut r = r.unwrap();
    r.play_from(0).unwrap();
    let out = r.render(4096);
    assert!(out[0][AT] > 0.1, "front left: {}", out[0][AT]);
    for c in right_side {
        assert!(out[c][AT].abs() < 1e-6, "{c}: {}", out[c][AT]);
    }

    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = bed;
    let stereo = tp.track(TrackKind::Aux, "Stereo Return", ChannelLayout::Stereo);
    let src = stereo_dc(&mut tp, 0.5, 0.5, 9600);
    let a = track_with(&mut tp, "A", ChannelLayout::Stereo, src, 9600);
    tp.project.track_mut(a).unwrap().output = OutputRouting::None;
    send_to(&mut tp, a, stereo, SendTap::PreFader, None);
    let t = tp.project.track_mut(stereo).unwrap();
    t.surround.stereo = StereoSource::Balance;
    t.pan = -1.0;
    assert!(
        tp.project
            .balanced_into_bed(tp.project.track(stereo).unwrap())
    );
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    let out = r.render(4096);
    assert!(out[0][AT] > 0.1, "front left: {}", out[0][AT]);
    for c in right_side {
        assert!(out[c][AT].abs() < 1e-6, "{c}: {}", out[c][AT]);
    }
}

/// A post-fader send with a pan of its own places the *source* there —
/// not the main mix's panned image balanced again (a mono track panned
/// hard right sent hard left reached the return as silence). Its level
/// still follows the fader and the mute; "as tapped" keeps the image.
#[test]
fn a_post_fader_send_with_its_own_pan_places_the_source() {
    let law = PanLaw::ConstantPower3dB;
    let build = |pan: Option<f32>, fader_db: f32| {
        let mut tp = TestProject::new(SR);
        let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Stereo);
        let src = tp.dc(1, 0.5, 9600);
        let a = track_with(&mut tp, "A", ChannelLayout::Mono, src, 9600);
        let t = tp.project.track_mut(a).unwrap();
        t.pan = 1.0;
        t.volume_db = fader_db;
        send_to(&mut tp, a, aux, SendTap::PostFader, pan);
        // The track into the master by name (the default would do too).
        let master = tp.master();
        let t = tp.project.track_mut(a).unwrap();
        t.output = OutputRouting::Track { track: master };
        (tp, a, aux)
    };
    // The track alone (hard right) plus the send placing it hard left.
    let (tp, _, _) = build(Some(-1.0), 0.0);
    let out = render(&tp, 256, 4096);
    assert!(
        approx(out[0][AT], 0.5, 1e-6),
        "the send, hard left: {}",
        out[0][AT]
    );
    assert!(
        approx(out[1][AT], 0.5, 1e-6),
        "the track, hard right: {}",
        out[1][AT]
    );
    // Positions in between by the law, the fader on top (post-fader).
    for pan in [-0.6f32, 0.0, 0.3, 1.0] {
        let (tp, _, _) = build(Some(pan), -6.020_6);
        let out = render(&tp, 256, 4096);
        let (gl, gr) = law.mono_gains(pan);
        let (l, r) = (0.25 * gl, 0.25 * gr + 0.25);
        assert!(
            approx(out[0][AT], l, 1e-5),
            "{pan}: L {} vs {l}",
            out[0][AT]
        );
        assert!(
            approx(out[1][AT], r, 1e-5),
            "{pan}: R {} vs {r}",
            out[1][AT]
        );
    }
    // As tapped: the main mix's image (hard right) at the fader's level.
    let (tp, _, _) = build(None, 0.0);
    let out = render(&tp, 256, 4096);
    assert_eq!((out[0][AT], out[1][AT]), (0.0, 1.0));
    // The track's mute silences its send too.
    let (mut tp, a, _) = build(Some(-1.0), 0.0);
    tp.project.track_mut(a).unwrap().mute = true;
    let out = render(&tp, 256, 4096);
    assert!(out.iter().flatten().all(|x| *x == 0.0));

    // A stereo track, dual-panned (sides swapped): its own pan balances
    // the source's sides, not the swapped image.
    let mut tp = TestProject::new(SR);
    let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Stereo);
    let src = stereo_dc(&mut tp, 0.6, 0.0, 9600);
    let s = track_with(&mut tp, "S", ChannelLayout::Stereo, src, 9600);
    let t = tp.project.track_mut(s).unwrap();
    t.pan_mode = PanMode::Dual;
    t.dual_pan = DualPan {
        left: 1.0,
        right: -1.0,
    };
    t.output = OutputRouting::None;
    send_to(&mut tp, s, aux, SendTap::PostFader, Some(0.0));
    let out = render(&tp, 256, 4096);
    assert!(approx(out[0][AT], 0.6, 1e-6) && approx(out[1][AT], 0.0, 1e-6));
}

/// Giving a send its own pan while playing (and taking it back) moves its
/// tap at once: the graph is rebuilt for it, with no glitch elsewhere.
#[test]
fn a_send_given_its_own_pan_while_playing_places_the_source() {
    let mut tp = TestProject::new(SR);
    let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Stereo);
    let src = tp.dc(1, 0.5, SR as usize);
    let a = track_with(&mut tp, "A", ChannelLayout::Mono, src, SR as usize);
    // Into a stereo bus kept silent: only the return is heard, its
    // post-fader image panned.
    let silent = tp.track(TrackKind::Bus, "Silent", ChannelLayout::Stereo);
    tp.project.track_mut(silent).unwrap().mute = true;
    tp.route(a, silent);
    tp.project.track_mut(a).unwrap().pan = 1.0;
    let send = send_to(&mut tp, a, aux, SendTap::PostFader, None);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(4096);
    assert_eq!((before[0][4095], before[1][4095]), (0.0, 0.5));
    let mut h = History::default();
    for (pan, want) in [(Some(-1.0), (0.5, 0.0)), (None, (0.0, 0.5))] {
        let impact = h
            .apply(
                &mut tp.project,
                Command::SetSendPan {
                    track: a,
                    send,
                    pan,
                },
            )
            .unwrap();
        assert_eq!(impact, faderframe_project::Impact::Params);
        r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
        let out = r.render(4096);
        assert!(
            approx(out[0][4095], want.0, 1e-6) && approx(out[1][4095], want.1, 1e-6),
            "{pan:?}: {} {}",
            out[0][4095],
            out[1][4095]
        );
    }
}
