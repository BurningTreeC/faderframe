#![allow(clippy::unwrap_used)]
//! Cue mixes (the performers' headphone mixes): every track's cue send
//! reaches the cue mix's own outputs at its level and pan, before (or
//! after) the fader and never muted — the main mix's fader, mute and solo
//! do not change what a performer hears —, under the cue mix's master
//! level and mute; the click plays there at the cue's level (the count-in
//! too); the outputs are heard with the master's (plugin delay
//! compensation), early with low-latency monitoring; a render has none.

mod common;

use common::{TestProject, hits};
use faderframe_audio::OwnedBuffers;
use faderframe_core::{ChannelLayout, CueId, ParameterId, TrackId, builtin, db_to_gain};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{ClickKind, ClickSounds, EngineConfig, MetronomeMode, RecordTarget};
use faderframe_project::{
    CueMix, CueSend, Impact, OutputRouting, PluginRef, PluginSlot, SavedParameter, SendTap,
    TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;

const SR: u32 = 48_000;
const BLOCK: usize = 256;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

/// A live engine (a render's builds no cue mixes) on a device with
/// `outputs` outputs.
fn live(tp: &TestProject, outputs: usize) -> OfflineRenderer {
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, outputs).unwrap();
    r.controller.plugins().set_realtime(true);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r
}

/// `frames` of every output (input `impulse_at`: a 1 on input 1 there).
fn play(
    r: &mut OfflineRenderer,
    outputs: usize,
    frames: usize,
    impulse_at: Option<usize>,
) -> Vec<Vec<f32>> {
    let mut bufs = OwnedBuffers::new(2, outputs, BLOCK);
    let mut out = vec![Vec::new(); outputs];
    let mut frame = 0;
    while frame < frames {
        bufs.set_frames(BLOCK);
        for c in 0..2 {
            bufs.input_mut(c).fill(0.0);
        }
        if let Some(at) = impulse_at.filter(|at| (frame..frame + BLOCK).contains(at)) {
            bufs.input_mut(0)[at - frame] = 1.0;
        }
        r.processor.process_device(&mut bufs);
        r.controller.collect_garbage();
        for (c, o) in out.iter_mut().enumerate() {
            o.extend_from_slice(bufs.output_ref(c));
        }
        frame += BLOCK;
    }
    out
}

/// A cue mix on outputs `first`.. (0-based) with nothing in it yet.
fn cue(tp: &mut TestProject, first: u16, layout: ChannelLayout) -> CueId {
    let id: CueId = tp.project.ids.allocate();
    tp.project.cue_mixes.push(CueMix::new(
        id,
        "Vocal Phones",
        TrackColor::palette(4),
        first,
        layout,
    ));
    id
}

fn cue_send(tp: &mut TestProject, track: TrackId, cue: CueId, level_db: f32, pan: f32) {
    let t = tp.project.track_mut(track).unwrap();
    t.cue_sends.retain(|s| s.cue != cue);
    t.cue_sends.push(CueSend {
        cue,
        level_db,
        pan,
        on: true,
    });
}

/// A mono track playing 0.5 for two seconds.
fn dc_track(tp: &mut TestProject, name: &str) -> TrackId {
    let t = tp.track(TrackKind::Audio, name, ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, SR as usize * 2);
    tp.clip(t, src, MusicalTime::ZERO, i64::from(SR) * 2);
    t
}

fn sync(r: &mut OfflineRenderer, tp: &TestProject, impact: Impact) {
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

/// A pre-fader cue send: its level and pan whatever the main fader, mute
/// and solo do; the cue mix's master level and mute over it.
#[test]
fn a_track_reaches_its_cue_at_its_level_and_pan_whatever_the_main_mix_does() {
    let mut tp = TestProject::new(SR);
    let vox = dc_track(&mut tp, "Vox");
    let other = tp.track(TrackKind::Audio, "Other", ChannelLayout::Mono);
    let c = cue(&mut tp, 2, ChannelLayout::Stereo);
    cue_send(&mut tp, vox, c, -6.0, -1.0);
    {
        let t = tp.project.track_mut(vox).unwrap();
        t.volume_db = -20.0;
        t.mute = true;
    }
    tp.project.track_mut(other).unwrap().solo = true;
    let mut r = live(&tp, 4);
    r.play_from(0).unwrap();
    let out = play(&mut r, 4, 8 * BLOCK, None);
    let at = 6 * BLOCK;
    let level = 0.5 * db_to_gain(-6.0);
    // Hard left in the cue mix; nothing in the main mix (muted, and another
    // track soloed).
    assert!(close(out[2][at], level), "{}", out[2][at]);
    assert_eq!(out[3][at], 0.0);
    assert_eq!(out[0][at], 0.0);
    assert_eq!(out[1][at], 0.0);

    // Centred: the −3 dB pan law, as the strip pans.
    cue_send(&mut tp, vox, c, -6.0, 0.0);
    sync(&mut r, &tp, Impact::Params);
    let out = play(&mut r, 4, 4 * BLOCK, None);
    let centre = level * std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(out[2][3 * BLOCK], centre), "{}", out[2][3 * BLOCK]);
    assert!(close(out[3][3 * BLOCK], centre));

    // The main mix back (unmuted, nothing soloed, another fader): the cue
    // mix does not move.
    {
        let t = tp.project.track_mut(vox).unwrap();
        t.mute = false;
        t.volume_db = 0.0;
    }
    tp.project.track_mut(other).unwrap().solo = false;
    sync(&mut r, &tp, Impact::Params);
    let out = play(&mut r, 4, 4 * BLOCK, None);
    assert!(close(out[2][3 * BLOCK], centre), "{}", out[2][3 * BLOCK]);
    assert!(close(
        out[0][3 * BLOCK],
        0.5 * std::f32::consts::FRAC_1_SQRT_2
    ));

    // The cue mix's master level and mute.
    tp.project.cue_mut(c).unwrap().level_db = -6.0;
    sync(&mut r, &tp, Impact::Params);
    let out = play(&mut r, 4, 4 * BLOCK, None);
    assert!(close(out[2][3 * BLOCK], centre * db_to_gain(-6.0)));
    tp.project.cue_mut(c).unwrap().mute = true;
    sync(&mut r, &tp, Impact::Params);
    let out = play(&mut r, 4, 4 * BLOCK, None);
    assert_eq!(out[2][3 * BLOCK], 0.0);
    assert_eq!(out[3][3 * BLOCK], 0.0);
    // The send off: silent there too.
    tp.project.cue_mut(c).unwrap().mute = false;
    tp.project.track_mut(vox).unwrap().cue_sends[0].on = false;
    sync(&mut r, &tp, Impact::Params);
    let out = play(&mut r, 4, 4 * BLOCK, None);
    assert_eq!(out[2][3 * BLOCK], 0.0);
}

/// A post-fader cue mix follows the main fader — but never the mute or a
/// solo elsewhere. A stereo source is balanced.
#[test]
fn a_post_fader_cue_follows_the_fader_not_the_mute() {
    let mut tp = TestProject::new(SR);
    let vox = dc_track(&mut tp, "Vox");
    let keys = tp.track(TrackKind::Audio, "Keys", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.25, SR as usize * 2);
    tp.clip(keys, src, MusicalTime::ZERO, i64::from(SR) * 2);
    let other = tp.track(TrackKind::Audio, "Other", ChannelLayout::Mono);
    let c = cue(&mut tp, 2, ChannelLayout::Stereo);
    tp.project.cue_mut(c).unwrap().tap = SendTap::PostFader;
    cue_send(&mut tp, vox, c, 0.0, 0.0);
    cue_send(&mut tp, keys, c, -6.0, 0.5);
    {
        let t = tp.project.track_mut(vox).unwrap();
        t.volume_db = -6.0;
        t.mute = true;
    }
    tp.project.track_mut(other).unwrap().solo = true;
    let mut r = live(&tp, 4);
    r.play_from(0).unwrap();
    let out = play(&mut r, 4, 8 * BLOCK, None);
    let at = 6 * BLOCK;
    let vox_in = 0.5 * db_to_gain(-6.0) * std::f32::consts::FRAC_1_SQRT_2;
    // Keys balanced half right: the left side at half.
    let keys_l = 0.25 * db_to_gain(-6.0) * 0.5;
    let keys_r = 0.25 * db_to_gain(-6.0);
    assert!(close(out[2][at], vox_in + keys_l), "{}", out[2][at]);
    assert!(close(out[3][at], vox_in + keys_r), "{}", out[3][at]);
    assert_eq!(out[0][at], 0.0, "the main mix hears only the soloed track");
}

/// The click in a cue mix: at its level under the cue's master level,
/// mono into a mono cue mix, off with the cue's switch or mute; the main
/// outputs' click is switched on its own.
#[test]
fn the_click_plays_in_a_cue_mix_at_its_level() {
    let mut tp = TestProject::new(SR);
    let stereo = cue(&mut tp, 2, ChannelLayout::Stereo);
    let mono = cue(&mut tp, 4, ChannelLayout::Mono);
    {
        let s = tp.project.cue_mut(stereo).unwrap();
        s.click_db = -6.0;
        s.level_db = -6.0;
    }
    tp.project.cue_mut(mono).unwrap().click = false;
    let mut r = live(&tp, 5);
    let m = r.controller.metronome();
    m.set_mode(MetronomeMode::Always);
    m.set_gain(0.5);
    m.set_level(ClickKind::Accent, 1.0);
    m.set_main(false);
    let sound = |v: f32| vec![[v, v]; 100];
    r.controller
        .set_click_sounds(ClickSounds::new(SR, sound(1.0), sound(0.8), sound(0.6)));
    r.play_from(0).unwrap();
    let out = play(&mut r, 5, 4 * BLOCK, None);
    let want = 0.5 * db_to_gain(-6.0) * db_to_gain(-6.0);
    assert!(close(out[2][0], want), "{}", out[2][0]);
    assert!(close(out[3][0], want));
    assert_eq!(out[0][0], 0.0, "not on the main outputs");
    assert_eq!(out[4][0], 0.0, "its click is off there");
    // The main outputs and the mono cue mix on; the stereo one muted.
    r.controller.metronome().set_main(true);
    tp.project.cue_mut(mono).unwrap().click = true;
    tp.project.cue_mut(stereo).unwrap().mute = true;
    sync(&mut r, &tp, Impact::Params);
    r.controller.transport(TransportCommand::Locate(0)).unwrap();
    let out = play(&mut r, 5, 4 * BLOCK, None);
    assert!(close(out[0][0], 0.5));
    assert!(close(out[4][0], 0.5), "mono: (L + R) / 2");
    assert_eq!(out[2][0], 0.0, "muted");
}

/// Counting in (the metronome off): the click reaches the cue mixes too.
#[test]
fn a_count_in_clicks_in_the_cue_mixes() {
    let mut tp = TestProject::new(SR);
    let c = cue(&mut tp, 2, ChannelLayout::Stereo);
    let _ = c;
    let mut r = live(&tp, 4);
    r.controller.metronome().set_mode(MetronomeMode::Off);
    r.controller.metronome().set_main(false);
    r.controller.set_count_in(true).unwrap();
    let _s = r
        .controller
        .begin_recording(
            vec![RecordTarget {
                track: TrackId(999_999),
                first_channel: 0,
                channels: 2,
            }],
            60_000,
            i64::MAX,
            2.0,
        )
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    r.play_from(0).unwrap();
    let out = play(&mut r, 4, 30_000, None);
    let click = out[2][Q..Q + 2_000]
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(click > 0.01, "the count-in in the headphones: {click}");
    assert!(out[0].iter().all(|v| *v == 0.0), "not on the main outputs");
}

/// A render (its own engine) builds no cue mixes: bit for bit what the
/// project renders without them, even with one on the master's outputs.
#[test]
fn renders_never_carry_cue_mixes() {
    let mut tp = TestProject::new(SR);
    let vox = dc_track(&mut tp, "Vox");
    tp.project.track_mut(vox).unwrap().volume_db = -10.0;
    let render = |tp: &TestProject| {
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
        r.play_from(0).unwrap();
        r.render(8 * BLOCK)
    };
    let without = render(&tp);
    let c = cue(&mut tp, 0, ChannelLayout::Stereo);
    cue_send(&mut tp, vox, c, 6.0, -1.0);
    let with = render(&tp);
    assert_eq!(without, with);
    assert!(with[0][6 * BLOCK] > 0.1);
}

fn delay(tp: &mut TestProject, track: TrackId, latency: u32) {
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
}

/// A mono track playing an impulse at quarter 1 (sample 24 000).
fn impulse_track(tp: &mut TestProject, name: &str) -> TrackId {
    let t = tp.track(TrackKind::Audio, name, ChannelLayout::Mono);
    let imp = tp.impulse(1, 64);
    tp.clip(t, imp, MusicalTime::from_quarters_i(1), 64);
    t
}

/// A cue mix is a heard output: what meets at the master meets there, on
/// the master's sample (a 6400-frame path and a dry one), pre- or
/// post-fader; and the master's outputs do not wait for it either.
#[test]
fn cue_mixes_are_heard_with_the_master() {
    for tap in [SendTap::PreFader, SendTap::PostFader] {
        let mut tp = TestProject::new(SR);
        let dry = impulse_track(&mut tp, "Dry");
        let late = impulse_track(&mut tp, "Late");
        delay(&mut tp, late, 6400);
        let c = cue(&mut tp, 2, ChannelLayout::Stereo);
        tp.project.cue_mut(c).unwrap().tap = tap;
        cue_send(&mut tp, dry, c, 0.0, 0.0);
        cue_send(&mut tp, late, c, 0.0, 0.0);
        let mut r = live(&tp, 4);
        assert_eq!(r.controller.graph_stats().output_latency, 6400);
        r.play_from(0).unwrap();
        let out = play(&mut r, 4, Q + 8192, None);
        assert_eq!(hits(&out[0], 1e-4), vec![Q + 6400], "{tap:?}: master");
        assert_eq!(hits(&out[2], 1e-4), vec![Q + 6400], "{tap:?}: cue");
        let both = 2.0 * std::f32::consts::FRAC_1_SQRT_2;
        assert!(close(out[2][Q + 6400], both), "{}", out[2][Q + 6400]);
    }
}

/// With low-latency monitoring a performer hears their live input in the
/// cue mix at once, the playback there still aligned with the master's.
#[test]
fn low_latency_monitoring_reaches_the_cue_mixes() {
    use faderframe_project::{InputRouting, MonitorMode};
    let mut tp = TestProject::new(SR);
    let playback = impulse_track(&mut tp, "Playback");
    delay(&mut tp, playback, 6400);
    let vocal = tp.track(TrackKind::Audio, "Vocal", ChannelLayout::Mono);
    {
        let v = tp.project.track_mut(vocal).unwrap();
        v.input = InputRouting::Hardware { first_channel: 0 };
        v.monitor = MonitorMode::Input;
        // Only in the cue mix (not the main mix).
        v.output = OutputRouting::None;
    }
    delay(&mut tp, vocal, 3000);
    let c = cue(&mut tp, 2, ChannelLayout::Stereo);
    cue_send(&mut tp, vocal, c, 0.0, 0.0);
    cue_send(&mut tp, playback, c, 0.0, 0.0);
    const INPUT_AT: usize = 30_000;
    let run = |low_latency: bool| {
        let mut r = live(&tp, 4);
        r.controller.set_low_latency_monitoring(low_latency);
        sync(&mut r, &tp, Impact::Graph);
        r.play_from(0).unwrap();
        let out = play(&mut r, 4, 45_000, Some(INPUT_AT));
        (hits(&out[2], 1e-4), hits(&out[0], 1e-4))
    };
    // Compensated: the input as late as the playback.
    let (cue_out, main) = run(false);
    assert_eq!(cue_out, vec![Q + 6400, INPUT_AT + 6400]);
    assert_eq!(main, vec![Q + 6400]);
    // Low latency: the input at once in the headphones, the playback
    // unchanged.
    let (cue_out, main) = run(true);
    assert_eq!(cue_out, vec![INPUT_AT, Q + 6400]);
    assert_eq!(main, vec![Q + 6400]);
}

/// A send's pan (an ordinary send too): a mono pre-fader send into a
/// stereo return by the pan law, unpanned (None) as before — at unity on
/// both sides.
#[test]
fn an_ordinary_send_can_be_panned() {
    let mut tp = TestProject::new(SR);
    let vox = dc_track(&mut tp, "Vox");
    let aux = tp.track(TrackKind::Aux, "Verb", ChannelLayout::Stereo);
    tp.project.track_mut(aux).unwrap().output = OutputRouting::Hardware { first_channel: 2 };
    tp.project.track_mut(vox).unwrap().output = OutputRouting::None;
    let id = tp.project.ids.allocate();
    tp.project
        .track_mut(vox)
        .unwrap()
        .sends
        .push(faderframe_project::AuxSend {
            id,
            target: aux,
            level_db: 0.0,
            tap: SendTap::PreFader,
            enabled: true,
            pan: None,
        });
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 4).unwrap();
    r.play_from(0).unwrap();
    let out = r.render(8 * BLOCK);
    // Unpanned: both sides at unity (the graph's channel rule).
    assert!(close(out[2][6 * BLOCK], 0.5));
    assert!(close(out[3][6 * BLOCK], 0.5));
    tp.project.track_mut(vox).unwrap().sends[0].pan = Some(1.0);
    sync(&mut r, &tp, Impact::Params);
    let out = r.render(4 * BLOCK);
    assert!(close(out[2][3 * BLOCK], 0.0), "{}", out[2][3 * BLOCK]);
    assert!(close(out[3][3 * BLOCK], 0.5));
    tp.project.track_mut(vox).unwrap().sends[0].pan = Some(0.0);
    sync(&mut r, &tp, Impact::Params);
    let out = r.render(4 * BLOCK);
    let centre = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(out[2][3 * BLOCK], centre));
    assert!(close(out[3][3 * BLOCK], centre));
}
