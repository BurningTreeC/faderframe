//! An interactive score's transitions (its preview): planned jumps of the
//! transport land on their frames — in any block size, and with the tracks
//! rendered ahead the same — a launch or a return to the arrangement on an
//! exact sample, and the transition's fades as heard.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio::DeviceBuffers;
use faderframe_audio_files::AudioData;
use faderframe_core::{ChannelLayout, ClipId, SceneId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::launch::{LaunchCommand, Quantize};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::transition::TransitionFade;
use faderframe_project::launcher::{Scene, SlotKey};
use faderframe_project::{
    AudioClip, Clip, ClipContent, ClipFades, Impact, PluginRef, PluginSlot, StretchSettings,
    TrackKind,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::{Cue, Cues, LoopRange, TransportCommand};
use std::time::Duration;

const SR: u32 = 48_000;
/// The ramp's length: every sample's value tells where it was played.
const LEN: usize = 400_000;

/// A stereo track whose clip's sample `i` is `(i + 1) / LEN`.
fn ramp_project() -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Score", ChannelLayout::Stereo);
    let ramp: Vec<f32> = (0..LEN).map(|i| (i + 1) as f32 / LEN as f32).collect();
    let src = tp.source(AudioData::from_channels(SR, vec![ramp.clone(), ramp]));
    tp.clip(t, src, MusicalTime::ZERO, LEN as i64);
    // Something to render ahead (unity).
    let id = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id,
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    (tp, t)
}

fn renderer(tp: &TestProject, block: usize, ahead: bool) -> OfflineRenderer {
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: 256,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, block, 2).unwrap();
    if ahead {
        r.controller
            .set_render_ahead(Some(Duration::from_millis(150)), 2);
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Graph)
            .unwrap();
    }
    r
}

/// Where each sample heard was played (the ramp read back; silence left
/// out), from `blocks` callbacks; paced callbacks let the anticipator keep
/// up as at a device.
fn heard_positions(r: &mut OfflineRenderer, blocks: usize, paced: bool, gain: f32) -> Vec<i64> {
    let mut out = Vec::new();
    let block = {
        let b = r.step();
        b.frames()
    };
    for _ in 0..blocks {
        let b = r.step();
        for v in b.output_ref(0) {
            if v.abs() > 1e-9 {
                out.push((v / gain * LEN as f32).round() as i64 - 1);
            }
        }
        if paced {
            std::thread::sleep(Duration::from_secs_f64(block as f64 / f64::from(SR)));
        }
    }
    out
}

/// The positions heard run on one after another but at the jumps listed
/// (`(from, to)`: `from` heard last before `to`), which all happen.
fn assert_runs(heard: &[i64], jumps: &[(i64, i64)], what: &str) {
    let mut expected = jumps.iter();
    let mut next = expected.next();
    for w in heard.windows(2) {
        if w[1] == w[0] + 1 {
            continue;
        }
        match next {
            Some(&(from, to)) if w[0] == from && w[1] == to => next = expected.next(),
            _ => panic!("{what}: an unexpected jump {} → {}", w[0], w[1]),
        }
    }
    assert!(next.is_none(), "{what}: the jump {next:?} did not come");
}

fn gain_of(r: &mut OfflineRenderer) -> f32 {
    // The track's level: what the first sample played reads as.
    let b = r.step();
    let v = b.output_ref(0)[10];
    v / (11.0 / LEN as f32)
}

#[test]
fn planned_jumps_land_on_their_frames_in_any_block_size() {
    for block in [64, 333, 1024] {
        let (tp, _) = ramp_project();
        let mut r = renderer(&tp, block, false);
        // A state looping 0–48 000, the jump on its end to 96 000, which
        // loops 96 000–144 000; from there at 120 000 back to 10 000.
        r.controller
            .transport(TransportCommand::SetLoopRange(LoopRange::new(0, 48_000)))
            .unwrap();
        r.controller
            .transport(TransportCommand::SetLoopEnabled(true))
            .unwrap();
        r.play_from(0).unwrap();
        let g = gain_of(&mut r);
        r.controller
            .set_cues(Cues::new(&[
                Cue {
                    at: 48_000,
                    to: 96_000,
                    then_loop: Some((LoopRange::new(96_000, 144_000), true)),
                },
                Cue {
                    at: 120_000,
                    to: 10_000,
                    then_loop: Some((None, false)),
                },
            ]))
            .unwrap();
        let heard = heard_positions(&mut r, 100_000 / block, false, g);
        assert_runs(
            &heard,
            &[(47_999, 96_000), (119_999, 10_000)],
            &format!("block {block}"),
        );
        let t = r.controller.transport_snapshot();
        assert_eq!((t.cues_taken, t.cues_waiting), (2, 0));
        assert!(!t.looping, "the last jump left the loop off");
    }
}

#[test]
fn tracks_rendered_ahead_take_the_same_jumps() {
    let (tp, t) = ramp_project();
    let g = {
        let mut plain = renderer(&tp, 256, false);
        plain.play_from(0).unwrap();
        gain_of(&mut plain)
    };
    for tries in 1..=3 {
        let mut r = renderer(&tp, 256, true);
        assert!(r.controller.ahead_tracks().contains(&t), "rendered ahead");
        r.controller
            .transport(TransportCommand::SetLoopRange(LoopRange::new(0, 48_000)))
            .unwrap();
        r.controller
            .transport(TransportCommand::SetLoopEnabled(true))
            .unwrap();
        r.play_from(0).unwrap();
        // Primed and playing.
        for _ in 0..400 {
            let primed = r.step().output_ref(0)[0].abs() > 1e-9;
            std::thread::sleep(Duration::from_secs_f64(256.0 / f64::from(SR)));
            if primed {
                break;
            }
        }
        let pos = r.controller.transport_snapshot().position;
        // Planned well beyond the lookahead (150 ms): on the loop's end, to
        // 200 000 and its loop.
        assert!(pos < 48_000 - 12_000, "{pos}");
        r.controller
            .set_cues(Cues::new(&[Cue {
                at: 48_000,
                to: 200_000,
                then_loop: Some((LoopRange::new(200_000, 260_000), true)),
            }]))
            .unwrap();
        let heard = heard_positions(&mut r, 360, true, g);
        let misses = r.controller.ahead_misses();
        if misses > 0 && tries < 3 {
            eprintln!("try {tries}: {misses} late blocks (a busy machine): again");
            continue;
        }
        assert_eq!(misses, 0, "the anticipator kept up");
        assert_runs(&heard, &[(47_999, 200_000)], "rendered ahead");
        assert_eq!(r.controller.transport_snapshot().cues_taken, 1);
        return;
    }
}

#[test]
fn a_fade_out_reaches_silence_on_the_jump_and_the_destination_fades_in() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Pad", ChannelLayout::Stereo);
    let dc = tp.dc(2, 0.5, LEN);
    tp.clip(t, dc, MusicalTime::ZERO, LEN as i64);
    let mut r = renderer(&tp, 256, false);
    r.play_from(0).unwrap();
    let level = r.step().output_ref(0)[10];
    assert!(level > 0.1);
    r.controller
        .set_cues(Cues::new(&[Cue {
            at: 20_000,
            to: 100_000,
            then_loop: None,
        }]))
        .unwrap();
    r.controller
        .transition_fade(Some(TransitionFade {
            at: 20_000,
            out_frames: 1_000,
            in_frames: 2_000,
            cue: true,
        }))
        .unwrap();
    let mut out = Vec::new();
    let mut positions = Vec::new();
    for _ in 0..120 {
        let b = r.step();
        out.extend_from_slice(b.output_ref(0));
        positions.push(r.controller.transport_snapshot().position);
    }
    // Output frame k (from frame 256) played position 256 + k up to the
    // jump: no graph latency here.
    let at = 20_000 - 256;
    assert!(
        (out[at - 1_001] - level).abs() < 1e-6,
        "full before the fade"
    );
    assert!(
        (out[at - 500] - level * 0.5).abs() < 1e-4,
        "{}",
        out[at - 500]
    );
    assert!(out[at].abs() < 1e-6, "silent on the jump");
    assert!((out[at + 1_000] - level * 0.5).abs() < 1e-4);
    assert!((out[at + 2_500] - level).abs() < 1e-6, "full again");
}

/// A launcher slot holding `content` on `track` (a new scene).
fn slot(tp: &mut TestProject, track: TrackId, content: ClipContent) -> u64 {
    let scene: SceneId = tp.project.ids.allocate();
    tp.project.launcher.scenes.push(Scene {
        id: scene,
        name: "Scene".into(),
    });
    let id: ClipId = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track,
            name: "launched".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content,
        },
    );
    let key = SlotKey { track, scene };
    tp.project.launcher.slots.insert(key, id);
    key.hash()
}

#[test]
fn a_scene_launches_on_its_sample_and_the_arrangement_comes_back_on_another() {
    for block in [64, 333] {
        let mut tp = TestProject::new(SR);
        let t = tp.track(TrackKind::Audio, "Loop", ChannelLayout::Mono);
        let dc = tp.dc(1, 0.1, LEN);
        tp.clip(t, dc, MusicalTime::ZERO, LEN as i64);
        let launched = tp.dc(1, 0.4, 48_000);
        let s = slot(
            &mut tp,
            t,
            ClipContent::Audio(AudioClip {
                source: launched,
                source_offset: 0,
                length: 48_000,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
                envelope: None,
            }),
        );
        let mut r = renderer(&tp, block, false);
        r.play_from(0).unwrap();
        let first = r.step().output_ref(0)[0];
        let g = first / 0.1;
        r.controller
            .launch(LaunchCommand::Launch {
                track: t,
                slot: s,
                quantize: Quantize::At(30_011),
                legato: false,
                repeat: 0,
            })
            .unwrap();
        let mut out = vec![first; block];
        while out.len() < 40_000 {
            out.extend_from_slice(r.step().output_ref(0));
        }
        // Once it plays (a return asked for earlier replaces the launch
        // waiting).
        r.controller
            .launch(LaunchCommand::Arrangement {
                quantize: Quantize::At(70_007),
            })
            .unwrap();
        while out.len() < 90_000 {
            out.extend_from_slice(r.step().output_ref(0));
        }
        for (i, v) in out.iter().enumerate() {
            let want = if (30_011..70_007).contains(&i) {
                0.4
            } else {
                0.1
            };
            assert!(
                (v - want * g).abs() < 1e-5,
                "block {block}: frame {i}: {v} (want {})",
                want * g
            );
        }
    }
}

#[test]
fn gains_beside_the_faders_scale_their_tracks() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Drums", ChannelLayout::Stereo);
    let dc = tp.dc(2, 0.5, LEN);
    tp.clip(t, dc, MusicalTime::ZERO, LEN as i64);
    let mut r = renderer(&tp, 256, false);
    r.play_from(0).unwrap();
    for _ in 0..4 {
        r.step();
    }
    let full = r.step().output_ref(0)[200];
    r.controller
        .set_track_trims(&tp.project, [(t, 0.25)].into_iter().collect())
        .unwrap();
    // The strip glides to it: a while later it is there.
    for _ in 0..40 {
        r.step();
    }
    let trimmed = r.step().output_ref(0)[200];
    assert!((trimmed - full * 0.25).abs() < 1e-4, "{full} → {trimmed}");
    r.controller
        .set_track_trims(&tp.project, Default::default())
        .unwrap();
    for _ in 0..40 {
        r.step();
    }
    assert!((r.step().output_ref(0)[200] - full).abs() < 1e-4);
}
