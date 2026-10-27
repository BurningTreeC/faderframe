#![allow(clippy::unwrap_used)]
//! Counting in before playback (`TransportCommand::PlayAfterCountIn`): the
//! transport stands while the click counts the beats before the position
//! it plays from, then plays from there on the frame after the count-in,
//! inside the callback; the click's beats run on into the playback's,
//! every one a beat after the last as heard — behind plugin latency too.

mod common;

use common::TestProject;
use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_core::{ChannelLayout, ParameterId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{ClickKind, ClickSound, ClickSounds, EngineConfig, MetronomeMode};
use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;
use faderframe_transport::{LoopRange, TransportCommand};

const SR: u32 = 48_000;
/// Not a divisor of a beat: the count-in ends inside a callback.
const BLOCK: usize = 112;
/// A beat and a bar at 120 BPM in 4/4.
const BEAT: usize = 24_000;
const BAR: usize = 96_000;
const DC: f32 = 0.25;

/// A track playing a constant from the start (behind `latency` frames of
/// plugin latency).
fn renderer(latency: u32) -> OfflineRenderer {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Song", ChannelLayout::Stereo);
    let dc = tp.dc(2, DC, 400_000);
    tp.clip(t, dc, MusicalTime::ZERO, 400_000);
    if latency > 0 {
        let id = tp.project.ids.allocate();
        tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
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
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap()
}

/// `blocks` callbacks' left output.
fn play(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers, blocks: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * BLOCK);
    for _ in 0..blocks {
        bufs.set_frames(BLOCK);
        r.processor.process_device(bufs);
        r.controller.collect_garbage();
        out.extend_from_slice(bufs.output(0));
    }
    out
}

/// The click of `kind` at the metronome's default levels, as it is mixed in.
fn click(kind: ClickKind) -> Vec<f32> {
    let sounds = ClickSounds::synth(ClickSound::default(), SR);
    let shared = faderframe_engine::MetronomeShared::default();
    let gain = shared.level(kind) * shared.gain();
    sounds
        .sound(kind)
        .iter()
        .map(|f| 0.0 + f[0] * gain)
        .collect()
}

/// Whether `out` from `at` is `base` plus the click `c`, sample for sample.
fn clicks_at(out: &[f32], at: usize, base: f32, c: &[f32]) -> bool {
    c.iter()
        .enumerate()
        .all(|(j, v)| (out[at + j] - (base + v)).abs() < 1e-6)
}

#[test]
fn a_count_in_clicks_every_beat_then_plays_from_the_start_on_its_frame() {
    for latency in [0usize, 6_400] {
        let mut r = renderer(latency as u32);
        let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
        // Clicks while playing too: the count-in's run on into them.
        r.controller.metronome().set_mode(MetronomeMode::Always);
        // Stopped for a while (the graph runs).
        let before = play(&mut r, &mut bufs, 20);
        assert!(before.iter().all(|v| *v == 0.0));
        r.controller.transport(TransportCommand::Locate(0)).unwrap();
        r.controller
            .transport(TransportCommand::PlayAfterCountIn(BAR as u64))
            .unwrap();
        // The first callback: counting in, standing at the start.
        let mut out = play(&mut r, &mut bufs, 1);
        let t = r.controller.transport_snapshot();
        assert!(!t.playing && t.started());
        assert_eq!((t.position, t.counting_in), (0, (BAR - BLOCK) as u64));
        let blocks = (BAR + 2 * BEAT + latency) / BLOCK + 1;
        out.extend(play(&mut r, &mut bufs, blocks));
        let t = r.controller.transport_snapshot();
        assert!(t.playing && t.counting_in == 0);
        assert_eq!(
            t.position as usize,
            out.len() - BAR,
            "playing from the start since the frame after the count-in"
        );
        let (accent, beat) = (click(ClickKind::Accent), click(ClickKind::Beat));
        let l = latency;
        // Nothing of the arrangement before it is heard: the count-in's
        // clicks alone, a beat apart, the first on the bar line before the
        // start (an accent).
        assert!(clicks_at(&out, l, 0.0, &accent), "latency {l}: the first");
        for b in 1..4 {
            assert!(
                clicks_at(&out, l + b * BEAT, 0.0, &beat),
                "latency {l}: count-in beat {b}"
            );
        }
        let silent = |from: usize, to: usize| out[from..to].iter().all(|v| *v == 0.0);
        assert!(silent(0, l), "nothing before the first click");
        for b in 0..4 {
            let end = l + b * BEAT + accent.len().max(beat.len());
            assert!(
                silent(end, l + (b + 1) * BEAT),
                "latency {l}: after beat {b}"
            );
        }
        // Bar 1's downbeat exactly a beat after the last count-in beat, with
        // the song from the same frame on.
        assert!(
            clicks_at(&out, l + BAR, DC, &accent),
            "latency {l}: bar 1 on the frame after the count-in: {:?}",
            &out[l + BAR - 2..l + BAR + 4]
        );
        assert!(clicks_at(&out, l + BAR + BEAT, DC, &beat));
        assert!(
            (out[l + BAR + BEAT / 2] - DC).abs() < 1e-6,
            "the song plays on"
        );
    }
}

/// While counting in, the play history (what the playhead is drawn from)
/// says so: the start, not playing, the frames still to count.
#[test]
fn the_heard_clock_tells_the_count_in() {
    let mut r = renderer(6_400);
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    play(&mut r, &mut bufs, 10);
    r.controller.transport(TransportCommand::Locate(0)).unwrap();
    r.controller
        .transport(TransportCommand::PlayAfterCountIn(BAR as u64))
        .unwrap();
    play(&mut r, &mut bufs, 200);
    let snap = r.controller.heard_snapshot().unwrap();
    // The last callback started 199 callbacks into the count-in; 6400
    // frames of latency: heard is 199 × 112 − 6400 frames into it.
    let into = 199 * BLOCK - 6_400;
    let heard = snap.heard(0.0).unwrap();
    assert!(!heard.playing);
    assert_eq!(heard.pos, 0);
    assert_eq!(heard.count_in, (BAR - into) as u64);
    // Further on: past its end the start plays (ahead of what was
    // processed, as a picture is drawn ahead).
    let later = snap.heard_ahead((BAR + 1_000) as f64).unwrap();
    assert!(later.playing && later.count_in == 0);
    assert_eq!(later.pos, (into + 1_000) as i64);
}

/// Stop while counting in: the count-in is over, nothing plays after it,
/// its clicks stop; Play while counting changes nothing.
#[test]
fn stop_cancels_a_count_in_and_play_keeps_it() {
    let mut r = renderer(0);
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    r.controller.metronome().set_mode(MetronomeMode::Off);
    r.controller.transport(TransportCommand::Locate(0)).unwrap();
    r.controller
        .transport(TransportCommand::PlayAfterCountIn(BAR as u64))
        .unwrap();
    play(&mut r, &mut bufs, 100);
    r.controller.transport(TransportCommand::Play).unwrap();
    play(&mut r, &mut bufs, 100);
    let t = r.controller.transport_snapshot();
    assert_eq!(
        t.counting_in,
        (BAR - 200 * BLOCK) as u64,
        "Play kept counting"
    );
    assert!(!t.playing);
    // Stopped half way: never plays, the clicks end with it.
    r.controller.transport(TransportCommand::Stop).unwrap();
    let out = play(&mut r, &mut bufs, (2 * BAR) / BLOCK);
    let t = r.controller.transport_snapshot();
    assert!(!t.started() && t.position == 0);
    assert!(out.iter().all(|v| *v == 0.0), "silence after the stop");
}

/// Click only during the count-in: the metronome counts in, then keeps
/// quiet through the take — through a loop wrap and after the stop too
/// (the clicks still heard then are the take's) — whatever its mode; the
/// next start without a count-in clicks again.
#[test]
fn the_click_can_keep_quiet_after_counting_in() {
    for latency in [0usize, 6_400] {
        let l = latency;
        let mut r = renderer(latency as u32);
        let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
        r.controller.metronome().set_mode(MetronomeMode::Always);
        r.controller.metronome().set_count_in_only(true);
        r.controller
            .transport(TransportCommand::SetLoopRange(LoopRange::new(
                0, BAR as i64,
            )))
            .unwrap();
        r.controller
            .transport(TransportCommand::SetLoopEnabled(true))
            .unwrap();
        r.controller
            .transport(TransportCommand::PlayAfterCountIn(BAR as u64))
            .unwrap();
        let blocks = (2 * BAR + BAR / 2 + latency) / BLOCK;
        let out = play(&mut r, &mut bufs, blocks);
        assert!(r.controller.transport_snapshot().playing);
        let accent = click(ClickKind::Accent);
        assert!(clicks_at(&out, l, 0.0, &accent), "latency {l}: counted in");
        // The song alone from bar 1 on, through the loop's wrap.
        assert!(
            out[l + BAR..].iter().all(|v| (*v - DC).abs() < 1e-6),
            "latency {l}: no click after the count-in"
        );
        // Stopped: what is still on its way is the take's, no click.
        r.controller.transport(TransportCommand::Stop).unwrap();
        let tail = play(&mut r, &mut bufs, (BEAT + latency) / BLOCK);
        assert!(
            tail.iter().all(|v| *v == 0.0 || (*v - DC).abs() < 1e-6),
            "latency {l}: no click after the stop"
        );
        // Played without a count-in: the metronome's mode again.
        r.controller.transport(TransportCommand::Locate(0)).unwrap();
        r.controller.transport(TransportCommand::Play).unwrap();
        let out = play(&mut r, &mut bufs, (BEAT + latency) / BLOCK + 1);
        assert!(
            clicks_at(&out, l, DC, &accent),
            "latency {l}: clicks again on bar 1"
        );
    }
}
