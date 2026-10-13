#![allow(clippy::unwrap_used)]
//! Plugin delay compensation end to end: impulses through paths of unequal
//! latency meet on the same sample wherever they are summed (tracks,
//! buses, sends, outputs, beds), automation behind latency follows its
//! audio (across loop wraps too), the click and what the UI is told is
//! heard come as late as the music — whatever the block size.

mod common;

use common::{TestProject, hits};
use faderframe_audio_files::AudioData;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{ChannelLayout, ParameterId, SendId, SurroundFormat, TrackId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, MetronomeMode};
use faderframe_project::{
    AuxSend, Command, History, Impact, OutputRouting, PluginRef, PluginSlot, SavedParameter,
    SendTap, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

fn render(tp: &TestProject, block: usize, outputs: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), block, outputs).unwrap();
    r.play_from(0).unwrap();
    r.render(frames)
}

/// A latency probe of `latency` frames on `track`'s inserts.
fn delay(tp: &mut TestProject, track: TrackId, latency: u32) -> faderframe_core::PluginInstanceId {
    let id = tp.project.ids.allocate();
    tp.project
        .track_mut(track)
        .unwrap()
        .inserts
        .push(PluginSlot {
            id,
            plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
            bypass: false,
            parameters: vec![SavedParameter {
                id: ParameterId(0),
                value: f64::from(latency),
            }],
            state: None,
            sidechain: None,
        });
    id
}

fn lane(tp: &mut TestProject, track: TrackId, target: AutomationTarget, points: &[(f64, f64)]) {
    let curve = AutomationCurve::from_points(
        points
            .iter()
            .map(|&(q, value)| AutomationPoint {
                time: MusicalTime::from_quarters(q),
                value,
                shape: CurveShape::Step,
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

/// A mono track playing an impulse at quarter 1 (sample 24 000).
fn impulse_track(tp: &mut TestProject, name: &str) -> TrackId {
    let t = tp.track(TrackKind::Audio, name, ChannelLayout::Mono);
    let imp = tp.impulse(1, 64);
    tp.clip(t, imp, MusicalTime::from_quarters_i(1), 64);
    t
}

fn send(tp: &mut TestProject, from: TrackId, to: TrackId, tap: SendTap) {
    let s = AuxSend {
        id: SendId(tp.project.ids.allocate::<SendId>().raw()),
        target: to,
        level_db: 0.0,
        tap,
        enabled: true,
    };
    tp.project.track_mut(from).unwrap().sends.push(s);
}

/// Where the impulses landed: one sample, every path summed on it.
fn one_hit(out: &[f32]) -> usize {
    let h = hits(out, 1e-4);
    assert_eq!(h.len(), 1, "one sample, not several: {h:?}");
    h[0]
}

/// 6400 frames (about the linear phase EQ at resolution 1 with a spectral
/// band) against a dry track: one hit 6400 late, both on it, for blocks
/// of every size — 1, odd, typical, larger than the latency.
#[test]
fn a_6400_frame_path_and_a_dry_one_meet_on_the_same_sample() {
    let mut tp = TestProject::new(SR);
    let a = impulse_track(&mut tp, "Dry");
    let b = impulse_track(&mut tp, "Linear");
    delay(&mut tp, b, 6400);
    let want = Q + 6400;
    for block in [1usize, 37, 64, 256, 1000, 8192] {
        let out = render(&tp, block, 2, Q + 8192 + 64);
        assert_eq!(one_hit(&out[0]), want, "block {block}");
        // Constant power centre: each mono impulse at 1/√2 per side.
        let both = 2.0 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0][want] - both).abs() < 1e-5, "{}", out[0][want]);
    }
    let _ = a;
}

/// A bus with several inputs, nested buses, a dry track beside them: the
/// latencies add along each path and every path meets at the master.
#[test]
fn nested_buses_add_their_latencies_and_meet_the_dry_path() {
    let mut tp = TestProject::new(SR);
    let outer = tp.track(TrackKind::Bus, "Outer", ChannelLayout::Stereo);
    let inner = tp.track(TrackKind::Bus, "Inner", ChannelLayout::Stereo);
    tp.route(inner, outer);
    delay(&mut tp, inner, 1000);
    delay(&mut tp, outer, 500);
    let a = impulse_track(&mut tp, "Into inner");
    delay(&mut tp, a, 128);
    tp.route(a, inner);
    let b = impulse_track(&mut tp, "Into inner too");
    tp.route(b, inner);
    let c = impulse_track(&mut tp, "Into outer");
    tp.route(c, outer);
    let _d = impulse_track(&mut tp, "Dry");
    // 128 + 1000 + 500, serial.
    let r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    assert_eq!(r.controller.graph_stats().output_latency, 1628);
    let out = render(&tp, 256, 2, Q + 4096);
    assert_eq!(one_hit(&out[0]), Q + 1628);
    assert!((out[0][Q + 1628] - 4.0 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
}

/// Pre- and post-fader sends into a return with latency of its own meet
/// the track's direct path and a dry track at the master; the pre-fader
/// tap ignores the fader, still on time.
#[test]
fn sends_into_a_latent_return_meet_the_direct_paths() {
    for tap in [SendTap::PostFader, SendTap::PreFader, SendTap::PreFx] {
        let mut tp = TestProject::new(SR);
        let ret = tp.track(TrackKind::Aux, "Reverb", ChannelLayout::Stereo);
        delay(&mut tp, ret, 3000);
        let a = impulse_track(&mut tp, "Vocal");
        delay(&mut tp, a, 200);
        send(&mut tp, a, ret, tap);
        let _dry = impulse_track(&mut tp, "Dry");
        let out = render(&tp, 128, 2, Q + 4096);
        let at = one_hit(&out[0]);
        // The send taps after the vocal's 200 (pre-FX: before it — the
        // return's path is then 3000 and the vocal's own 200 waits).
        let want = Q + if tap == SendTap::PreFx { 3000 } else { 3200 };
        assert_eq!(at, want, "{tap:?}");
    }
}

/// Parallel chains that split and meet again (a container's chains) and a
/// bed: every speaker channel carries its impulse on the same sample.
#[test]
fn a_bed_keeps_its_channels_together() {
    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Surround(SurroundFormat::S51);
    let c = impulse_track(&mut tp, "Centre");
    let vbap = |azimuth: f32| faderframe_core::surround::SurroundPan {
        panner: faderframe_core::surround::SurroundPanner::Vbap,
        azimuth,
        ..Default::default()
    };
    tp.project.track_mut(c).unwrap().surround = vbap(0.0);
    let s = impulse_track(&mut tp, "Surround");
    delay(&mut tp, s, 777);
    tp.project.track_mut(s).unwrap().surround = vbap(110.0);
    let out = render(&tp, 256, 6, Q + 2048);
    let mut speakers = 0;
    for (c, ch) in out.iter().enumerate() {
        let h = hits(ch, 1e-6);
        assert!(h.iter().all(|&i| i == Q + 777), "channel {c}: {h:?}");
        speakers += usize::from(!h.is_empty());
    }
    assert!(speakers >= 2, "both tracks reach the bed");
}

/// Two hardware outputs — the master on 1/2 with a latent track, a direct
/// out on 3/4 — are heard together.
#[test]
fn hardware_outputs_are_heard_together() {
    let mut tp = TestProject::new(SR);
    let a = impulse_track(&mut tp, "To master");
    delay(&mut tp, a, 2500);
    let b = impulse_track(&mut tp, "Direct out");
    tp.project.track_mut(b).unwrap().output = OutputRouting::Hardware { first_channel: 2 };
    let out = render(&tp, 64, 4, Q + 4096);
    assert_eq!(one_hit(&out[0]), Q + 2500);
    assert_eq!(
        one_hit(&out[2]),
        Q + 2500,
        "the direct out waits for the master"
    );
}

/// A fader step after a 6400-frame plugin moves with the audio it acts on
/// (not 6400 frames early), the same for any block size.
#[test]
fn fader_automation_behind_latency_follows_its_audio() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, Q * 4);
    tp.clip(t, src, MusicalTime::ZERO, (Q * 4) as i64);
    delay(&mut tp, t, 6400);
    // Silent until quarter 1, full from there.
    lane(
        &mut tp,
        t,
        AutomationTarget::TrackVolume,
        &[(0.0, -144.0), (1.0, 0.0)],
    );
    for block in [64usize, 333, 1024] {
        let out = render(&tp, block, 2, Q * 2 + 6400);
        // The audio of quarter 1 comes out at Q + 6400: silent before
        // (where the old reading would have opened it), full after.
        assert!(
            out[0][Q + 6400 - 64].abs() < 1e-4,
            "early: {}",
            out[0][Q + 6400 - 64]
        );
        assert!(out[0][Q + 100].abs() < 1e-4, "not opened 6400 early");
        assert!(
            out[0][Q + 6400 + 64].abs() > 0.3,
            "{}",
            out[0][Q + 6400 + 64]
        );
        let opened = out[0].iter().position(|v| v.abs() > 1e-4).unwrap();
        // (The fader ramps over an automation step of 32 frames.)
        assert!(
            (Q + 6400 - 32..=Q + 6400).contains(&opened),
            "block {block}: {opened}"
        );
    }
}

/// A plugin's parameter automation behind a latent plugin (a utility's
/// mute after 4000 frames) follows its audio too.
#[test]
fn plugin_automation_behind_latency_follows_its_audio() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, Q * 4);
    tp.clip(t, src, MusicalTime::ZERO, (Q * 4) as i64);
    delay(&mut tp, t, 4000);
    let utility = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id: utility,
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    lane(
        &mut tp,
        t,
        AutomationTarget::PluginParameter {
            plugin: utility,
            parameter: ParameterId(faderframe_plugin_host::devices::utility::id::MUTE),
        },
        &[(0.0, 0.0), (1.0, 1.0)],
    );
    let out = render(&tp, 256, 2, Q * 2);
    assert!(out[0][Q + 2000].abs() > 0.3, "still playing at Q + 2000");
    assert!(out[0][Q + 4000 + 1000].abs() < 1e-4, "muted with its audio");
}

/// Looping 0…10 000 with a fader that is shut for the first half: behind
/// 3000 frames of latency, the first 3000 frames after each wrap are the
/// loop's end — open — and the start after that is shut.
#[test]
fn automation_behind_latency_reads_back_across_a_loop_wrap() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, Q * 4);
    tp.clip(t, src, MusicalTime::ZERO, (Q * 4) as i64);
    delay(&mut tp, t, 3000);
    let at = |s: i64| tp.project.timeline.to_musical(s, f64::from(SR)).quarters();
    let (half, end) = (at(5000), at(10_000));
    lane(
        &mut tp,
        t,
        AutomationTarget::TrackVolume,
        &[(0.0, -144.0), (half, 0.0)],
    );
    tp.project.loop_range =
        faderframe_project::MusicalRange::new(MusicalTime::ZERO, MusicalTime::from_quarters(end));
    tp.project.loop_enabled = true;
    let out = render(&tp, 256, 2, 30_000);
    let loud = |i: usize| out[0][i].abs() > 0.3;
    let quiet = |i: usize| out[0][i].abs() < 1e-4;
    // Pass two: frames 10 000… carry positions 7000… (open) until 13 000,
    // then 0… (shut) until 18 000, then open again.
    for i in [10_100, 12_000, 12_900, 18_100, 22_900] {
        assert!(loud(i), "{i}: {}", out[0][i]);
    }
    for i in [13_100, 15_000, 17_900, 23_100] {
        assert!(quiet(i), "{i}: {}", out[0][i]);
    }
}

/// The click comes as late as the music: with 6400 frames of latency in
/// the graph its first beat starts 6400 frames in.
#[test]
fn the_click_is_heard_with_the_music() {
    let mut tp = TestProject::new(SR);
    let a = impulse_track(&mut tp, "A");
    delay(&mut tp, a, 6400);
    for latency in [0usize, 6400] {
        let mut tp = TestProject::new(SR);
        let a = impulse_track(&mut tp, "A");
        if latency > 0 {
            delay(&mut tp, a, latency as u32);
        }
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
        r.controller.metronome().set_mode(MetronomeMode::Always);
        r.play_from(0).unwrap();
        let out = r.render(Q + 8000);
        let first = out[0].iter().position(|v| v.abs() > 1e-6).unwrap();
        assert_eq!(first, latency + 1, "the beat at 0 (its sine starts at 0)");
        // The beat at quarter 1 lands on the impulse's sample.
        let beat = out[0][Q + latency..Q + latency + 40]
            .iter()
            .any(|v| v.abs() > 0.01);
        assert!(beat, "the second click with the impulse");
    }
}

/// What the UI is told is heard is what comes out: the frame an impulse
/// at position 7000 leaves the graph, behind 6400 frames of latency, maps
/// back to 7000.
#[test]
fn the_heard_position_is_the_one_coming_out() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let mut v = vec![0.0f32; 10_000];
    v[7000] = 1.0;
    let src = tp.source(AudioData::from_channels(SR, vec![v]));
    tp.clip(t, src, MusicalTime::ZERO, 10_000);
    delay(&mut tp, t, 6400);
    let block = 512;
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), block, 2).unwrap();
    r.play_from(0).unwrap();
    let mut out = Vec::new();
    let mut found = None;
    while found.is_none() && out.len() < 40_000 {
        let chunk = r.render(block);
        let at = out.len();
        out.extend_from_slice(&chunk[0]);
        if let Some(i) = chunk[0].iter().position(|v| v.abs() > 1e-4) {
            found = Some((at + i, r.controller.heard_snapshot().unwrap()));
        }
    }
    let (frame, snap) = found.unwrap();
    assert_eq!(frame, 13_400);
    // The device's latency is not the graph's business: frames since the
    // callback as the device hands them out.
    let heard = snap.heard((frame as i64 - snap.frame) as f64).unwrap();
    assert_eq!(heard.pos, 7000);
    assert!(heard.playing);
    // Before the first 6400 frames come out, the start is heard, stopped.
    assert!(!snap.heard(-(snap.frame as f64)).unwrap().playing);
}

/// A locate: until its audio comes out the old place is heard, and the
/// controller says where the transport went.
#[test]
fn a_locate_is_announced_until_it_is_heard() {
    let mut tp = TestProject::new(SR);
    let t = impulse_track(&mut tp, "A");
    delay(&mut tp, t, 6400);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 512, 2).unwrap();
    r.play_from(0).unwrap();
    r.render(20_480);
    r.controller
        .transport(faderframe_transport::TransportCommand::Locate(96_000))
        .unwrap();
    r.render(512);
    let snap = r.controller.heard_snapshot().unwrap();
    // At this callback's start: 20 480 − 6400 frames played → 14 080.
    let now = snap.heard(0.0).unwrap();
    assert_eq!(now.pos, 14_080);
    assert_ne!(now.jumps, snap.jumps, "the locate is not heard yet");
    assert_eq!(snap.jumped_to, 96_000);
    // 6400 frames on, the new place.
    let later = snap.heard(6400.0).unwrap();
    assert_eq!((later.pos, later.jumps), (96_000, snap.jumps));
}

/// Adding latency to a track while running (a graph rebuild) keeps the
/// paths aligned with the new figure.
#[test]
fn a_latency_change_rebuilds_the_alignment() {
    let mut tp = TestProject::new(SR);
    let _a = impulse_track(&mut tp, "A");
    let b = impulse_track(&mut tp, "B");
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    assert_eq!(r.controller.graph_stats().output_latency, 0);
    let mut h = History::default();
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(0),
            value: 1500.0,
        }],
        state: None,
        sidechain: None,
    };
    let impact = h
        .apply(
            &mut tp.project,
            Command::InsertPlugin {
                track: b,
                index: 0,
                slot,
            },
        )
        .unwrap();
    assert_eq!(impact, Impact::Graph);
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    assert_eq!(r.controller.graph_stats().output_latency, 1500);
    r.play_from(0).unwrap();
    let out = r.render(Q + 4096);
    assert_eq!(one_hit(&out[0]), Q + 1500);
    assert_eq!(
        r.controller.graph_latency(),
        1500,
        "what the audio thread runs"
    );
}

/// The built-in EQ's reported latency is its delay: linear phase at
/// resolution 1 (4352) with a spectral band (its 2048-frame frame and the
/// 512-frame hop its work is spread over) — 6912 — measured from
/// an impulse through the engine (alone: where it really comes out), then
/// against a dry track (compensated onto the same sample).
#[test]
fn the_linear_phase_eq_with_a_spectral_band_is_6912_late_and_compensated() {
    use faderframe_plugin_host::eq::{Field, band_id, global, global_id};
    let with_eq = |dry: bool| {
        let mut tp = TestProject::new(SR);
        if dry {
            impulse_track(&mut tp, "Dry");
        }
        let eq = impulse_track(&mut tp, "EQ");
        let set = |id: ParameterId, value: f64| SavedParameter { id, value };
        let id = tp.project.ids.allocate();
        tp.project.track_mut(eq).unwrap().inserts.push(PluginSlot {
            id,
            plugin: PluginRef::builtin(builtin::EQ, "EQ"),
            bypass: false,
            parameters: vec![
                set(global_id(global::PHASE), 1.0),
                set(global_id(global::QUALITY), 1.0),
                set(band_id(0, Field::Enabled), 1.0),
                set(band_id(0, Field::Spectral), 1.0),
            ],
            state: None,
            sidechain: None,
        });
        let r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
        assert_eq!(r.controller.graph_stats().output_latency, 6912);
        render(&tp, 256, 2, Q + 6912 + 4096).swap_remove(0)
    };
    let peak = |out: &[f32]| {
        out.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, v)| (i, *v))
            .unwrap()
    };
    // Alone: its response is centred 6912 frames late (linear phase: a
    // symmetric kernel around its peak), nothing comes out early.
    let alone = with_eq(false);
    let (at, v) = peak(&alone);
    assert_eq!(at, Q + 6912, "the reported latency is the delay");
    assert!(alone[..Q + 6912 - 3000].iter().all(|x| x.abs() < 1e-3));
    // With a dry track: both peaks on that sample.
    let both = with_eq(true);
    let (at2, v2) = peak(&both);
    assert_eq!(at2, Q + 6912);
    assert!(
        (v2 - (v + std::f32::consts::FRAC_1_SQRT_2)).abs() < 1e-3,
        "{v2} = {v} + the dry impulse"
    );
}

/// How much later than its meter each track is heard: a dry track beside
/// a 6400-frame one waits 6400 at the master; the latent one none; a track
/// into a bus with latency waits for the bus's; the master none.
#[test]
fn every_meter_knows_how_much_later_it_is_heard() {
    let mut tp = TestProject::new(SR);
    let dry = impulse_track(&mut tp, "Dry");
    let late = impulse_track(&mut tp, "Late");
    delay(&mut tp, late, 6400);
    let bus = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Stereo);
    delay(&mut tp, bus, 1000);
    let into = impulse_track(&mut tp, "Into bus");
    tp.route(into, bus);
    let r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    let c = &r.controller;
    assert_eq!(c.graph_stats().output_latency, 6400);
    assert_eq!(c.meter_delay(dry), 6400);
    assert_eq!(c.meter_delay(late), 0);
    // Into the bus: the bus's 1000 after it, then 5400 waiting at the
    // master — 6400 either way.
    assert_eq!(c.meter_delay(into), 6400);
    assert_eq!(c.meter_delay(bus), 5400);
    assert_eq!(c.meter_delay(tp.master()), 0);
}

/// A live input heard through a track with 3000 frames of plugin latency,
/// in a project whose playback has 6400: compensated, the input comes out
/// with the playback (6400 late); with low-latency monitoring the track's
/// effect is bypassed and its output not held back — the input comes out
/// at once, while the playback stays compensated (6400) and so does a
/// render.
#[test]
fn low_latency_monitoring_hears_the_input_at_once_and_keeps_playback_aligned() {
    use faderframe_audio::OwnedBuffers;
    use faderframe_project::{InputRouting, MonitorMode};
    let mut tp = TestProject::new(SR);
    let playback = impulse_track(&mut tp, "Playback");
    delay(&mut tp, playback, 6400);
    let _dry = impulse_track(&mut tp, "Dry");
    let vocal = tp.track(TrackKind::Audio, "Vocal", ChannelLayout::Mono);
    {
        let v = tp.project.track_mut(vocal).unwrap();
        v.input = InputRouting::Hardware { first_channel: 0 };
        v.monitor = MonitorMode::Input;
    }
    delay(&mut tp, vocal, 3000);
    // An input impulse at frame 30 000 of the stream.
    const INPUT_AT: usize = 30_000;
    let run = |low_latency: bool| {
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
        r.controller.set_low_latency_monitoring(low_latency);
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Graph)
            .unwrap();
        assert_eq!(r.controller.graph_stats().output_latency, 6400);
        assert_eq!(
            r.controller.low_latency_tracks().contains(&vocal),
            low_latency
        );
        r.play_from(0).unwrap();
        let mut bufs = OwnedBuffers::new(2, 2, 256);
        let mut out = Vec::new();
        let mut frame = 0;
        while frame < 45_000 {
            bufs.set_frames(256);
            for c in 0..2 {
                bufs.input_mut(c).fill(0.0);
            }
            if (frame..frame + 256).contains(&INPUT_AT) {
                bufs.input_mut(0)[INPUT_AT - frame] = 1.0;
            }
            r.processor.process_device(&mut bufs);
            out.extend_from_slice(bufs.output_ref(0));
            frame += 256;
        }
        hits(&out, 1e-4)
    };
    // Compensated: playback and dry on Q + 6400, the input 6400 late.
    assert_eq!(run(false), vec![Q + 6400, INPUT_AT + 6400]);
    // Low latency: the input at once, the playback unchanged.
    assert_eq!(run(true), vec![INPUT_AT, Q + 6400]);
    // A render never monitors low-latency (its own engine).
    let out = render(&tp, 256, 2, Q + 8000);
    assert_eq!(one_hit(&out[0]), Q + 6400);
}

/// A latency changing while sound goes out (a plugin with latency
/// inserted mid-play): the output fades out, stays silent while the new
/// delay lines fill, and fades back in — no step anywhere. A change that
/// moves no path's timing (another track) goes in without a dip.
#[test]
fn a_latency_change_while_playing_dips_instead_of_clicking() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, SR as usize * 4);
    tp.clip(t, src, MusicalTime::ZERO, i64::from(SR) * 4);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(8192);
    let level = before[0][8000];
    assert!(level > 0.3, "{level}");
    // Another track (no latency anywhere): no dip.
    let mut h = History::default();
    let other = tp.track(TrackKind::Audio, "Other", ChannelLayout::Mono);
    let impact = Impact::Graph;
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let same = r.render(4096);
    assert!(
        same[0].iter().all(|v| (v - level).abs() < 1e-6),
        "no dip for a change that moves nothing"
    );
    let _ = other;
    // A 1500-frame plugin on the playing track.
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(0),
            value: 1500.0,
        }],
        state: None,
        sidechain: None,
    };
    let impact = h
        .apply(
            &mut tp.project,
            Command::InsertPlugin {
                track: t,
                index: 0,
                slot,
            },
        )
        .unwrap();
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let after = r.render(8192);
    let out = &after[0];
    let biggest_step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(biggest_step < 0.01, "a step of {biggest_step}");
    assert!(out.iter().any(|v| v.abs() < 1e-6), "silent while filling");
    assert!((out[8000] - level).abs() < 1e-6, "back: {}", out[8000]);
}

/// Low-latency monitoring all the way out: a live vocal into a bus that
/// also plays a track, a 6400-frame track elsewhere, an effect with 1000
/// frames of latency on the master. The master's effect is bypassed (over
/// the limit) and the bus's playback held back where the vocal joins it:
/// the vocal comes out at once, the playback all together 6400 late. With
/// a limit above the master's effect it stays, and the vocal comes 1000
/// late.
#[test]
fn low_latency_monitoring_clears_the_whole_way_out() {
    use faderframe_audio::OwnedBuffers;
    use faderframe_project::{InputRouting, MonitorMode};
    let mut tp = TestProject::new(SR);
    let bus = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Mono);
    let keys = impulse_track(&mut tp, "Keys");
    tp.route(keys, bus);
    let drums = impulse_track(&mut tp, "Drums");
    delay(&mut tp, drums, 6400);
    let vocal = tp.track(TrackKind::Audio, "Vocal", ChannelLayout::Mono);
    {
        let v = tp.project.track_mut(vocal).unwrap();
        v.input = InputRouting::Hardware { first_channel: 0 };
        v.monitor = MonitorMode::Input;
    }
    tp.route(vocal, bus);
    let master = tp.master();
    delay(&mut tp, master, 1000);
    const INPUT_AT: usize = 40_000;
    let run = |limit: u32| {
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
        r.controller.set_low_latency_monitoring(true);
        r.controller.set_low_latency_limit(limit);
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Graph)
            .unwrap();
        r.play_from(0).unwrap();
        let mut bufs = OwnedBuffers::new(2, 2, 256);
        let mut out = Vec::new();
        let mut frame = 0;
        while frame < 50_000 {
            bufs.set_frames(256);
            for c in 0..2 {
                bufs.input_mut(c).fill(0.0);
            }
            if (frame..frame + 256).contains(&INPUT_AT) {
                bufs.input_mut(0)[INPUT_AT - frame] = 1.0;
            }
            r.processor.process_device(&mut bufs);
            out.extend_from_slice(bufs.output_ref(0));
            frame += 256;
        }
        (
            hits(&out, 1e-4),
            r.controller.bypassed_for_monitoring().len(),
        )
    };
    // Limit 0: the master's effect bypassed, the vocal at once.
    let (heard, bypassed) = run(0);
    assert_eq!(bypassed, 1, "the master's effect");
    assert_eq!(heard, vec![Q + 6400, INPUT_AT]);
    // Limit 2000 frames: the master's effect stays (1000 frames).
    let (heard, bypassed) = run(2000);
    assert_eq!(bypassed, 0);
    assert_eq!(heard, vec![Q + 6400 + 1000, INPUT_AT + 1000]);
}

/// A second latency change while the first one's dip is still silent
/// (its delay lines filling) goes in too, and one during the fade-in fades
/// out from where the gain is: no step, and the last graph plays.
#[test]
fn latency_changes_in_quick_succession_all_go_in_without_a_click() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, SR as usize * 4);
    tp.clip(t, src, MusicalTime::ZERO, i64::from(SR) * 4);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let level = r.render(8192)[0][8000];
    let mut h = History::default();
    let mut insert = |tp: &mut TestProject, r: &mut OfflineRenderer| {
        let slot = PluginSlot {
            id: tp.project.ids.allocate(),
            plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
            bypass: false,
            parameters: vec![SavedParameter {
                id: ParameterId(0),
                value: 1500.0,
            }],
            state: None,
            sidechain: None,
        };
        let impact = h
            .apply(
                &mut tp.project,
                Command::InsertPlugin {
                    track: t,
                    index: 0,
                    slot,
                },
            )
            .unwrap();
        r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    };
    let mut out = Vec::new();
    // The first: faded out, the new graph filling for 1500 frames…
    insert(&mut tp, &mut r);
    out.extend_from_slice(&r.render(768)[0]);
    // …when the second arrives.
    insert(&mut tp, &mut r);
    out.extend_from_slice(&r.render(4096)[0]);
    assert_eq!(r.controller.graph_latency(), 3000, "the second went in");
    // A third while fading in.
    let fill_and_most_of_the_fade = 3000 + 96;
    out.extend_from_slice(&r.render(fill_and_most_of_the_fade)[0]);
    insert(&mut tp, &mut r);
    out.extend_from_slice(&r.render(16384)[0]);
    let biggest_step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(biggest_step < 0.01, "a step of {biggest_step}");
    assert_eq!(r.controller.graph_latency(), 4500, "the last graph plays");
    assert!((out[out.len() - 1] - level).abs() < 1e-6, "back");
}
