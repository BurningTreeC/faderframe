//! The clip launcher: a launched clip starts on the quantised position,
//! loops over its length, stops on the next one, and "back to the
//! arrangement" brings the arrangement back — sample-accurately, in any
//! block size; launched MIDI clips play their notes and end the notes
//! they leave behind.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{ChannelLayout, ClipId, SceneId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::launch::{LaunchCommand, Quantize};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::launcher::{Scene, SlotKey};
use faderframe_project::{
    AudioClip, Clip, ClipContent, ClipFades, Impact, MidiClip, MidiNote, PluginRef, PluginSlot,
    StretchSettings, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// A beat and a bar at 120 BPM, 4/4.
const BEAT: usize = 24_000;
const BAR: usize = 96_000;

/// Put `content` in a launcher slot of `track` (a new scene).
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

fn audio(source: faderframe_core::AudioSourceId, length: usize) -> ClipContent {
    ClipContent::Audio(AudioClip {
        source,
        source_offset: 0,
        length: length as i64,
        gain_db: 0.0,
        fades: ClipFades::default(),
        stretch: StretchSettings::Off,
        reversed: false,
        warp: None,
        pitch: None,
        effects: None,
    })
}

/// An audio track with a 0.1 DC clip in the arrangement and a one-beat
/// ramp in a launcher slot.
fn ramp_project() -> (TestProject, TrackId, u64, Vec<f32>) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Loop", ChannelLayout::Mono);
    let dc = tp.dc(1, 0.1, 8 * BAR);
    tp.clip(t, dc, MusicalTime::ZERO, 8 * BAR as i64);
    let ramp: Vec<f32> = (0..BEAT)
        .map(|i| 0.2 + 0.6 * i as f32 / BEAT as f32)
        .collect();
    let src = tp.source(AudioData::from_channels(SR, vec![ramp.clone()]));
    let s = slot(&mut tp, t, audio(src, BEAT));
    (tp, t, s, ramp)
}

/// Render at least `frames` frames in whole blocks; everything rendered
/// (channel 0).
fn take(r: &mut OfflineRenderer, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    while out.len() < frames {
        out.extend_from_slice(r.step().output_ref(0));
    }
    out
}

fn renderer(tp: &TestProject, block: usize) -> OfflineRenderer {
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: 256,
        ..EngineConfig::default()
    };
    OfflineRenderer::new(&tp.project, &tp.sources, config, block, 2).unwrap()
}

#[test]
fn a_launched_clip_starts_on_the_bar_loops_and_stops() {
    for block in [64, 333, 1024] {
        let (tp, t, s, ramp) = ramp_project();
        let mut r = renderer(&tp, block);
        r.play_from(0).unwrap();
        let mut out = take(&mut r, 10_000);
        // The arrangement plays; its level gives the track's gain.
        let g = out[5_000] / 0.1;
        assert!(g > 0.1, "the arrangement plays ({})", out[5_000]);
        r.controller
            .launch(LaunchCommand::Launch {
                track: t,
                slot: s,
                quantize: Quantize::Bars(1),
            })
            .unwrap();
        let more = take(&mut r, BAR + 3 * BEAT - out.len());
        out.extend_from_slice(&more);
        // Up to the bar: the arrangement, from it the ramp, looping.
        for (i, v) in out.iter().enumerate().skip(10_000) {
            let want = if i < BAR { 0.1 } else { ramp[(i - BAR) % BEAT] };
            assert!(
                (v - want * g).abs() < 1e-4,
                "block {block}: frame {i}: {v} (want {})",
                want * g
            );
        }
        let status = r.controller.launch_status();
        assert_eq!(status[0].playing, Some((s, BAR as i64)));
        // Stopped on the next beat: silence (not the arrangement).
        r.controller
            .launch(LaunchCommand::Stop {
                track: t,
                quantize: Quantize::Beat,
            })
            .unwrap();
        let at = out.len();
        let tail = take(&mut r, 2 * BEAT);
        let next_beat = (at.div_ceil(BEAT) * BEAT).max(at);
        for (k, v) in tail.iter().enumerate() {
            let i = at + k;
            if i >= next_beat {
                assert!(v.abs() < 1e-6, "block {block}: frame {i} stopped: {v}");
            }
        }
        // Back to the arrangement: at once.
        r.controller
            .launch(LaunchCommand::BackToArrangement)
            .unwrap();
        let back = take(&mut r, block);
        assert!((back[block - 1] - 0.1 * g).abs() < 1e-4);
        assert!(r.controller.launch_status()[0].arrangement);
    }
}

#[test]
fn launching_while_stopped_plays_from_the_start_of_playback() {
    let (tp, t, s, ramp) = ramp_project();
    let mut r = renderer(&tp, 128);
    r.controller
        .launch(LaunchCommand::Launch {
            track: t,
            slot: s,
            quantize: Quantize::Bars(1),
        })
        .unwrap();
    take(&mut r, 256);
    r.play_from(BAR as i64 / 2).unwrap();
    let out = take(&mut r, 2 * BEAT);
    // Playback starts with the clip's first sample (its gain: unity).
    assert!(out[0] > 0.0);
    let g = out[0] / ramp[0];
    for (i, v) in out.iter().enumerate() {
        assert!((v - g * ramp[i % BEAT]).abs() < 1e-4, "frame {i}: {v}");
    }
    // Stopping the transport stops the clip; the track then stays silent.
    r.controller
        .transport(faderframe_transport::TransportCommand::Stop)
        .unwrap();
    take(&mut r, 256);
    assert_eq!(r.controller.launch_status()[0].playing, None);
    r.play_from(0).unwrap();
    let out = take(&mut r, 1024);
    assert!(out.iter().all(|v| v.abs() < 1e-6));
}

#[test]
fn launcher_clips_play_from_disk() {
    let dir = std::env::temp_dir().join(format!("ff-engine-launch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("loop.wav");
    let ramp: Vec<f32> = (0..BEAT)
        .map(|i| 0.1 + 0.5 * i as f32 / BEAT as f32)
        .collect();
    faderframe_audio_files::write_wav(
        &path,
        &[ramp],
        SR,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Loop", ChannelLayout::Mono);
    let src = tp.stream(&path);
    let s = slot(&mut tp, t, audio(src, BEAT));
    let mut r = renderer(&tp, 512);
    // The playhead far from the clip's pages: they are loaded anyway.
    r.play_from(40 * BAR as i64).unwrap();
    r.controller
        .launch(LaunchCommand::Launch {
            track: t,
            slot: s,
            quantize: Quantize::None,
        })
        .unwrap();
    take(&mut r, 512);
    let out = take(&mut r, BEAT);
    assert!(out.iter().any(|v| v.abs() > 0.05));
    assert_eq!(r.stream_errors, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launched_midi_clip_plays_and_releases_the_arrangements_notes() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let synth = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    tp.project.track_mut(t).unwrap().inserts.push(synth);
    let note = |tp: &mut TestProject, start: f64, length: f64| MidiNote {
        id: tp.project.ids.allocate(),
        start: MusicalTime::from_quarters(start),
        length: MusicalTime::from_quarters(length),
        key: 60,
        velocity: 110,
        channel: 0,
        muted: false,
    };
    // A long note in the arrangement, a short one in the launched clip.
    let held = note(&mut tp, 0.0, 32.0);
    let id: ClipId = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: t,
            name: "Pad".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters_i(32),
                notes: vec![held],
                ..MidiClip::default()
            }),
        },
    );
    tp.project.track_mut(t).unwrap().clips.push(id);
    let short = note(&mut tp, 0.0, 0.25);
    let s = slot(
        &mut tp,
        t,
        ClipContent::Midi(MidiClip {
            length: MusicalTime::from_quarters_i(4),
            notes: vec![short],
            ..MidiClip::default()
        }),
    );
    let mut r = renderer(&tp, 256);
    r.play_from(0).unwrap();
    let before = take(&mut r, BAR / 2);
    r.controller
        .launch(LaunchCommand::Launch {
            track: t,
            slot: s,
            quantize: Quantize::Bars(1),
        })
        .unwrap();
    let out = take(&mut r, BAR / 2 + 2 * BAR);
    let energy = |a: usize, b: usize| out[a..b].iter().map(|v| v * v).sum::<f32>();
    // `out` starts `before.len()` after 0; the bar is `launch` into it.
    let launch = BAR - before.len();
    // The held note sounds up to the bar; after it only the short note (a
    // sixteenth per bar, looping), so the end of each bar is quiet.
    assert!(energy(0, launch) > 1.0);
    let quiet = |bar: usize| energy(launch + bar * BAR + 3 * BAR / 4, launch + (bar + 1) * BAR);
    let loud = |bar: usize| energy(launch + bar * BAR, launch + bar * BAR + BEAT / 2);
    for bar in 0..2 {
        assert!(loud(bar) > 0.1, "bar {bar} plays the note: {}", loud(bar));
        assert!(
            quiet(bar) < loud(bar) * 1e-3,
            "bar {bar} quiet: {} vs {}",
            quiet(bar),
            loud(bar)
        );
    }
}

#[test]
fn a_new_launcher_track_is_played_live() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Loop", ChannelLayout::Mono);
    let dc = tp.dc(1, 0.25, BAR);
    let mut r = renderer(&tp, 256);
    let s = slot(&mut tp, t, audio(dc, BAR));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Timeline)
        .unwrap();
    r.play_from(0).unwrap();
    r.controller
        .launch(LaunchCommand::Launch {
            track: t,
            slot: s,
            quantize: Quantize::None,
        })
        .unwrap();
    let out = take(&mut r, 4096);
    assert!(out[4000] > 0.1, "{}", out[4000]);
}

#[test]
fn follow_actions_move_on_exactly_on_time() {
    use faderframe_project::launcher::{FollowAction, FollowKind};
    for block in [64, 333] {
        let mut tp = TestProject::new(SR);
        let t = tp.track(TrackKind::Audio, "Loop", ChannelLayout::Mono);
        let ramp: Vec<f32> = (0..BEAT)
            .map(|i| 0.2 + 0.6 * i as f32 / BEAT as f32)
            .collect();
        let a = tp.source(AudioData::from_channels(SR, vec![ramp.clone()]));
        let b = tp.dc(1, 0.3, BEAT);
        let first = slot(&mut tp, t, audio(a, BEAT));
        let _second = slot(&mut tp, t, audio(b, BEAT));
        let scenes: Vec<SceneId> = tp.project.launcher.scenes.iter().map(|s| s.id).collect();
        // The first goes to the next after a bar; the second stops after
        // its length (a beat).
        for (scene, kind, bars) in [
            (scenes[0], FollowKind::Next, 1),
            (scenes[1], FollowKind::Stop, 0),
        ] {
            tp.project
                .launcher
                .follow
                .insert(SlotKey { track: t, scene }, FollowAction { kind, bars });
        }
        let mut r = renderer(&tp, block);
        r.play_from(0).unwrap();
        r.controller
            .launch(LaunchCommand::Launch {
                track: t,
                slot: first,
                quantize: Quantize::None,
            })
            .unwrap();
        let out = take(&mut r, BAR + 2 * BEAT);
        let g = out[10] / ramp[10];
        for (i, v) in out.iter().enumerate().take(BAR + 2 * BEAT) {
            let want = if i < BAR {
                ramp[i % BEAT]
            } else if i < BAR + BEAT {
                0.3
            } else {
                0.0
            };
            assert!(
                (v - g * want).abs() < 1e-4,
                "block {block}: frame {i}: {v} (want {})",
                g * want
            );
        }
        // A follow action is not shown as a launch waiting.
        assert!(r.controller.launch_status()[0].queued.is_none());
    }
}
