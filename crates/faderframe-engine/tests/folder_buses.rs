#![allow(clippy::unwrap_used)]
//! Folder buses: a folder that is a bus sums what it holds through its
//! own strip — exactly once, no doubling — with its fader, pan, mute,
//! solo, devices, sends and lanes; nested folder buses chain, plain
//! folders between are passed by, explicit routings stay; latency inside
//! is compensated where it meets the rest; a track moved in or out while
//! playing is heard there at once; a bed folder bus keeps its tracks'
//! spatial placement.

mod common;

use common::{TestProject, approx, hits};
use faderframe_audio_files::AudioData;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{ChannelLayout, ParameterId, SendId, SurroundFormat, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::{OfflineRenderer, render_project};
use faderframe_project::{
    AuxSend, Command, History, Impact, OutputRouting, PluginRef, PluginSlot, SavedParameter,
    SendTap, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;
/// A frame well inside the clips, past any start-up ramp.
const AT: usize = 3000;
/// The −3 dB law's centre.
const C: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

fn render(tp: &TestProject, frames: usize) -> Vec<Vec<f32>> {
    render_project(&tp.project, &tp.sources, config(), 256, 0, frames).unwrap()
}

/// A folder bus (stereo, to its default output) holding nothing yet.
fn folder_bus(tp: &mut TestProject, name: &str, layout: ChannelLayout) -> TrackId {
    let f = tp.track(TrackKind::Bus, name, layout);
    tp.project.track_mut(f).unwrap().folder_bus = true;
    f
}

/// A track playing `value` on each of `layout`'s channels for `frames`.
fn dc_track(
    tp: &mut TestProject,
    name: &str,
    layout: ChannelLayout,
    value: f32,
    frames: usize,
) -> TrackId {
    let t = tp.track(TrackKind::Audio, name, layout);
    let src = tp.dc(layout.channel_count(), value, frames);
    tp.clip(t, src, MusicalTime::ZERO, frames as i64);
    t
}

fn put_in(tp: &mut TestProject, track: TrackId, folder: TrackId) {
    tp.project.track_mut(track).unwrap().folder = Some(folder);
}

fn plugin(tp: &mut TestProject, track: TrackId, id: &str, params: &[(u32, f64)]) {
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, "Device"),
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
    };
    tp.project.track_mut(track).unwrap().inserts.push(slot);
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

/// Two tracks in the folder bus, one outside: what is inside reaches the
/// master once, through the bus's fader; made a plain folder (by the
/// command, undone and redone) it plays straight to the master again.
#[test]
fn tracks_in_a_folder_bus_reach_the_master_through_it_once() {
    let mut tp = TestProject::new(SR);
    let f = folder_bus(&mut tp, "Drums", ChannelLayout::Stereo);
    let kick = dc_track(&mut tp, "Kick", ChannelLayout::Mono, 0.5, 9600);
    let room = tp.track(TrackKind::Audio, "Room", ChannelLayout::Stereo);
    let src = tp.source(AudioData::from_channels(
        SR,
        vec![vec![0.2; 9600], vec![0.4; 9600]],
    ));
    tp.clip(room, src, MusicalTime::ZERO, 9600);
    let bass = dc_track(&mut tp, "Bass", ChannelLayout::Mono, 0.1, 9600);
    put_in(&mut tp, kick, f);
    put_in(&mut tp, room, f);
    let p = &tp.project;
    assert_eq!(p.output_target(p.track(kick).unwrap()), Some(f));
    assert_eq!(p.output_target(p.track(room).unwrap()), Some(f));
    assert_eq!(p.output_target(p.track(bass).unwrap()), p.master_id());
    assert_eq!(p.output_target(p.track(f).unwrap()), p.master_id());
    // Unity: the same as straight to the master (no doubling).
    let out = render(&tp, 4096);
    let (l, r) = (0.5 * C + 0.2 + 0.1 * C, 0.5 * C + 0.4 + 0.1 * C);
    assert!(approx(out[0][AT], l, 1e-6), "{} vs {l}", out[0][AT]);
    assert!(approx(out[1][AT], r, 1e-6), "{} vs {r}", out[1][AT]);
    // Its fader scales what it holds, and only that.
    tp.project.track_mut(f).unwrap().volume_db = -6.020_6;
    let out = render(&tp, 4096);
    let (l, r) = (
        (0.5 * C + 0.2) * 0.5 + 0.1 * C,
        (0.5 * C + 0.4) * 0.5 + 0.1 * C,
    );
    assert!(approx(out[0][AT], l, 1e-5), "{} vs {l}", out[0][AT]);
    assert!(approx(out[1][AT], r, 1e-5), "{} vs {r}", out[1][AT]);

    // A plain folder again: its fader does nothing, the tracks go on.
    let mut h = History::default();
    let off = Command::SetFolderBus {
        track: f,
        on: false,
        output: OutputRouting::None,
    };
    assert_eq!(off.impact(), Impact::Graph);
    h.apply(&mut tp.project, off).unwrap();
    let t = tp.project.track(f).unwrap();
    assert_eq!((t.kind, t.folder_bus), (TrackKind::Folder, false));
    assert!(t.is_folder() && !t.is_folder_bus());
    let out = render(&tp, 4096);
    let (l, r) = (0.5 * C + 0.2 + 0.1 * C, 0.5 * C + 0.4 + 0.1 * C);
    assert!(approx(out[0][AT], l, 1e-6) && approx(out[1][AT], r, 1e-6));
    // Undone: a bus again, with its settings.
    h.undo(&mut tp.project).unwrap();
    let t = tp.project.track(f).unwrap();
    assert!(t.is_folder_bus() && t.output == OutputRouting::Master);
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], (0.5 * C + 0.2) * 0.5 + 0.1 * C, 1e-5));
}

/// A folder bus is a bus: its pan (balance on a stereo bus, the law on a
/// mono one), its mute, its solo (and a solo elsewhere), its devices, its
/// sends and its lanes all reach what it holds.
#[test]
fn a_folder_bus_pans_mutes_solos_and_has_devices_sends_and_lanes() {
    let mut tp = TestProject::new(SR);
    let f = folder_bus(&mut tp, "Vox", ChannelLayout::Stereo);
    let lead = dc_track(&mut tp, "Lead", ChannelLayout::Mono, 0.5, Q * 2);
    put_in(&mut tp, lead, f);
    let other = dc_track(&mut tp, "Other", ChannelLayout::Mono, 0.1, Q * 2);
    let (o_l, o_r) = (0.1 * C, 0.1 * C);
    // Pan: hard left on its balance.
    tp.project.track_mut(f).unwrap().pan = -1.0;
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], 0.5 * C + o_l, 1e-6), "{}", out[0][AT]);
    assert!(approx(out[1][AT], o_r, 1e-6), "{}", out[1][AT]);
    tp.project.track_mut(f).unwrap().pan = 0.0;
    // A mono folder bus pans by the law.
    tp.project.track_mut(f).unwrap().layout = ChannelLayout::Mono;
    tp.project.track_mut(f).unwrap().pan = 1.0;
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], o_l, 1e-6), "{}", out[0][AT]);
    assert!(approx(out[1][AT], 0.5 + o_r, 1e-6), "{}", out[1][AT]);
    let t = tp.project.track_mut(f).unwrap();
    t.layout = ChannelLayout::Stereo;
    t.pan = 0.0;
    // Mute: what it holds goes quiet, the rest plays.
    tp.project.track_mut(f).unwrap().mute = true;
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], o_l, 1e-6) && approx(out[1][AT], o_r, 1e-6));
    tp.project.track_mut(f).unwrap().mute = false;
    // Solo: the folder bus and what it holds alone.
    tp.project.track_mut(f).unwrap().solo = true;
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], 0.5 * C, 1e-6) && approx(out[1][AT], 0.5 * C, 1e-6));
    tp.project.track_mut(f).unwrap().solo = false;
    // A track in it soloed: heard through the bus; the rest not.
    tp.project.track_mut(lead).unwrap().solo = true;
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], 0.5 * C, 1e-6));
    tp.project.track_mut(lead).unwrap().solo = false;
    // A solo elsewhere: it is silent.
    tp.project.track_mut(other).unwrap().solo = true;
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], o_l, 1e-6));
    tp.project.track_mut(other).unwrap().solo = false;
    tp.project.track_mut(other).unwrap().mute = true;

    // A device on it: a utility at −6 dB.
    plugin(
        &mut tp,
        f,
        builtin::GAIN,
        &[(faderframe_plugin_host::devices::utility::id::GAIN, -6.020_6)],
    );
    let out = render(&tp, 4096);
    assert!(approx(out[0][AT], 0.25 * C, 1e-4), "{}", out[0][AT]);
    // A send from it to a return: the return gets the bus's sum.
    let aux = tp.track(TrackKind::Aux, "Verb", ChannelLayout::Stereo);
    let id = SendId(tp.project.ids.allocate::<SendId>().raw());
    tp.project.track_mut(f).unwrap().sends.push(AuxSend {
        id,
        target: aux,
        level_db: 0.0,
        tap: SendTap::PostFader,
        enabled: true,
        pan: None,
    });
    let out = render(&tp, 4096);
    assert!(
        approx(out[0][AT], 0.5 * C, 1e-4),
        "bus + return: {}",
        out[0][AT]
    );
    tp.project.track_mut(f).unwrap().sends.clear();
    // Its fader lane: shut from the second quarter on.
    lane(
        &mut tp,
        f,
        AutomationTarget::TrackVolume,
        &[(0.0, 0.0), (1.0, faderframe_core::gain::SILENCE_DB as f64)],
    );
    let out = render(&tp, Q + 4096);
    assert!(approx(out[0][AT], 0.25 * C, 1e-4));
    assert!(out[0][Q + 2048..].iter().all(|x| x.abs() < 1e-6));
}

/// Folder buses in folder buses chain (each fader once); a plain folder
/// between is passed by; a MIDI track's output never goes to one.
#[test]
fn nested_folder_buses_chain_and_plain_folders_are_passed_by() {
    let mut tp = TestProject::new(SR);
    let outer = folder_bus(&mut tp, "Band", ChannelLayout::Stereo);
    let inner = folder_bus(&mut tp, "Drums", ChannelLayout::Stereo);
    let plain = tp.track(TrackKind::Folder, "Toms", ChannelLayout::Mono);
    let t = dc_track(&mut tp, "Tom", ChannelLayout::Mono, 0.8, 9600);
    let g = dc_track(&mut tp, "Guitar", ChannelLayout::Mono, 0.4, 9600);
    put_in(&mut tp, inner, outer);
    put_in(&mut tp, plain, inner);
    put_in(&mut tp, t, plain);
    put_in(&mut tp, g, outer);
    let midi = tp.track(TrackKind::Midi, "Notes", ChannelLayout::Stereo);
    put_in(&mut tp, midi, inner);
    let p = &tp.project;
    assert_eq!(p.output_target(p.track(t).unwrap()), Some(inner));
    assert_eq!(p.output_target(p.track(inner).unwrap()), Some(outer));
    assert_eq!(p.output_target(p.track(g).unwrap()), Some(outer));
    assert_eq!(p.output_target(p.track(outer).unwrap()), p.master_id());
    assert!(p.folder_bus_of(p.track(midi).unwrap()).is_none());
    assert!(!p.routing_loops());
    tp.project.track_mut(inner).unwrap().volume_db = -6.020_6;
    tp.project.track_mut(outer).unwrap().volume_db = -6.020_6;
    let out = render(&tp, 4096);
    let want = (0.8 * 0.25 + 0.4 * 0.5) * C;
    assert!(approx(out[0][AT], want, 1e-5), "{} vs {want}", out[0][AT]);
    assert!(approx(out[1][AT], want, 1e-5));
}

/// Explicit routings stay where they are: a track in a folder bus routed
/// to the master by name, or to another bus, passes it by (no doubling).
#[test]
fn explicit_routings_out_of_a_folder_bus_stay() {
    let mut tp = TestProject::new(SR);
    let f = folder_bus(&mut tp, "Folder", ChannelLayout::Stereo);
    tp.project.track_mut(f).unwrap().volume_db = faderframe_core::gain::SILENCE_DB;
    let other = tp.track(TrackKind::Bus, "Other Bus", ChannelLayout::Stereo);
    let a = dc_track(&mut tp, "A", ChannelLayout::Mono, 0.5, 9600);
    let b = dc_track(&mut tp, "B", ChannelLayout::Mono, 0.25, 9600);
    let c = dc_track(&mut tp, "C", ChannelLayout::Mono, 0.125, 9600);
    for t in [a, b, c] {
        put_in(&mut tp, t, f);
    }
    let master = tp.master();
    tp.route(a, master);
    tp.route(b, other);
    // C goes through the folder bus (shut): silent; A and B are heard once.
    let out = render(&tp, 4096);
    let want = (0.5 + 0.25) * C;
    assert!(approx(out[0][AT], want, 1e-6), "{} vs {want}", out[0][AT]);
}

/// 1000 frames of latency inside the folder bus (on a track in it) and
/// 500 on the bus: an impulse there and one outside meet on one sample.
#[test]
fn latency_inside_a_folder_bus_is_compensated_where_it_meets_the_rest() {
    let mut tp = TestProject::new(SR);
    let f = folder_bus(&mut tp, "Folder", ChannelLayout::Stereo);
    let inside = tp.track(TrackKind::Audio, "In", ChannelLayout::Mono);
    let outside = tp.track(TrackKind::Audio, "Out", ChannelLayout::Mono);
    for t in [inside, outside] {
        let imp = tp.impulse(1, 64);
        tp.clip(t, imp, MusicalTime::from_quarters_i(1), 64);
    }
    put_in(&mut tp, inside, f);
    plugin(&mut tp, inside, builtin::LATENCY_PROBE, &[(0, 1000.0)]);
    plugin(&mut tp, f, builtin::LATENCY_PROBE, &[(0, 500.0)]);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    assert_eq!(r.controller.graph_stats().output_latency, 1500);
    r.play_from(0).unwrap();
    let out = r.render(Q + 4096);
    let h = hits(&out[0], 1e-4);
    assert_eq!(h, vec![Q + 1500], "one sample, both paths summed on it");
    assert!(approx(out[0][Q + 1500], 2.0 * C, 1e-5));
}

/// Moving a track into a folder bus while playing (a parameter-sized
/// edit) is heard through the bus at once, and out of it again.
#[test]
fn a_track_moved_into_a_folder_bus_while_playing_plays_through_it() {
    let mut tp = TestProject::new(SR);
    let f = folder_bus(&mut tp, "Folder", ChannelLayout::Stereo);
    tp.project.track_mut(f).unwrap().volume_db = -6.020_6;
    let a = dc_track(&mut tp, "A", ChannelLayout::Mono, 0.5, SR as usize);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(4096);
    assert!(approx(before[0][4095], 0.5 * C, 1e-6));
    let mut h = History::default();
    for (folder, want) in [(Some(f), 0.25 * C), (None, 0.5 * C)] {
        let impact = h
            .apply(
                &mut tp.project,
                Command::SetTrackFolder { track: a, folder },
            )
            .unwrap();
        r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
        let out = r.render(4096);
        assert!(
            approx(out[0][4095], want, 1e-5),
            "{folder:?}: {} vs {want}",
            out[0][4095]
        );
    }
}

/// Routing that would loop through a folder bus is refused: a bus in the
/// folder bus that the folder bus feeds by name, a folder bus made of a
/// folder whose bus feeds a track in it.
#[test]
fn a_folder_bus_never_makes_a_loop() {
    let mut tp = TestProject::new(SR);
    let f = folder_bus(&mut tp, "Folder", ChannelLayout::Stereo);
    let sub = tp.track(TrackKind::Bus, "Sub", ChannelLayout::Stereo);
    tp.route(f, sub);
    let mut h = History::default();
    // Into the folder bus, Sub's default output would be the bus: a loop.
    assert!(
        h.apply(
            &mut tp.project,
            Command::SetTrackFolder {
                track: sub,
                folder: Some(f),
            },
        )
        .is_err()
    );
    assert_eq!(tp.project.track(sub).unwrap().folder, None);
    // Sub explicitly to the master: then it may go in.
    let master = tp.master();
    tp.route(sub, master);
    h.apply(
        &mut tp.project,
        Command::SetTrackFolder {
            track: sub,
            folder: Some(f),
        },
    )
    .unwrap();
    // Back to the default output (the folder bus): refused.
    assert!(
        h.apply(
            &mut tp.project,
            Command::SetTrackOutput {
                track: sub,
                output: OutputRouting::Master,
            },
        )
        .is_err()
    );
    assert!(!tp.project.routing_loops());
    // Rendered: no loop for the compiler to reject.
    render(&tp, 512);
}

/// A bed folder bus: tracks pan into it as they did into the master's
/// bed; it adds nothing of its own (unity).
#[test]
fn a_surround_folder_bus_keeps_its_tracks_placement() {
    let bed = ChannelLayout::Surround(SurroundFormat::S51);
    let make = |in_folder: bool| {
        let mut tp = TestProject::new(SR);
        let master = tp.master();
        tp.project.track_mut(master).unwrap().layout = bed;
        let f = folder_bus(&mut tp, "Folder", bed);
        let a = dc_track(&mut tp, "A", ChannelLayout::Mono, 0.5, 9600);
        let t = tp.project.track_mut(a).unwrap();
        t.surround.x = -0.6;
        t.surround.y = -0.3;
        let s = dc_track(&mut tp, "S", ChannelLayout::Stereo, 0.25, 9600);
        tp.project.track_mut(s).unwrap().surround.x = 0.4;
        if in_folder {
            put_in(&mut tp, a, f);
            put_in(&mut tp, s, f);
        }
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 6).unwrap();
        r.play_from(0).unwrap();
        r.render(4096)
    };
    let direct = make(false);
    let through = make(true);
    for c in 0..6 {
        assert!(
            approx(direct[c][AT], through[c][AT], 1e-6),
            "speaker {c}: {} vs {}",
            direct[c][AT],
            through[c][AT]
        );
    }
    assert!(direct.iter().any(|ch| ch[AT].abs() > 0.05));
}
