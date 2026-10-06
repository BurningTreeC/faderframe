//! The clip launcher in the session: scenes and slots are edited in undo
//! steps and saved; clips sent to it are copies kept out of the
//! arrangement; tracks take their launcher clips with them; launching
//! plays (starting the transport), and with "Record to Arrangement" what
//! played is written into the arrangement when the transport stops.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{SceneId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::launcher::{LaunchQuantize, SlotKey};
use faderframe_project::{ClipContent, Command};
use faderframe_session::launcher::LauncherOp;
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn op(s: &mut Session, op: LauncherOp) {
    s.dispatch(Action::Launcher(op)).unwrap();
}

fn scenes(s: &Session) -> Vec<SceneId> {
    s.project().launcher.scenes.iter().map(|s| s.id).collect()
}

fn run(s: &mut Session, millis: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(millis) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

#[test]
fn scenes_and_slots_are_edited_saved_and_kept_out_of_the_arrangement() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let chords = track(&s, "Chords");
    let arranged = |s: &Session, t| s.project().clips_of(t).len();
    let (drum_clips, chord_clips) = (arranged(&s, drums), arranged(&s, chords));
    // A clip of each track to the launcher, twice: each goes to its
    // track's first free slot, scenes made as needed.
    let send: Vec<_> = [drums, chords]
        .iter()
        .map(|t| s.project().clips_of(*t)[0].id)
        .collect();
    op(&mut s, LauncherOp::SendClips(send.clone()));
    op(&mut s, LauncherOp::SendClips(send.clone()));
    let sc = scenes(&s);
    assert_eq!(sc.len(), 2);
    let l = &s.project().launcher;
    assert_eq!(l.slots.len(), 4);
    for (k, c) in &l.slots {
        let clip = &s.project().clips[c];
        assert_eq!(clip.track, k.track);
        assert_eq!(clip.start, faderframe_timeline::MusicalTime::ZERO);
        assert!(!send.contains(c), "copies");
    }
    assert_eq!(
        arranged(&s, drums),
        drum_clips,
        "the arrangement keeps its clips"
    );
    assert_eq!(arranged(&s, chords), chord_clips);
    // Duplicated, renamed, moved, a new MIDI clip; audio tracks make none.
    op(&mut s, LauncherOp::DuplicateScene(sc[1]));
    let sc = scenes(&s);
    assert_eq!(sc.len(), 3);
    assert_eq!(s.project().launcher.slots.len(), 6);
    op(
        &mut s,
        LauncherOp::RenameScene {
            scene: sc[0],
            name: "Intro".into(),
        },
    );
    let to_chords = LauncherOp::MoveClip {
        from: SlotKey {
            track: drums,
            scene: sc[2],
        },
        to: SlotKey {
            track: chords,
            scene: sc[2],
        },
        copy: false,
    };
    assert!(s.dispatch(Action::Launcher(to_chords)).is_err());
    assert!(
        s.project().launcher.clip(drums, sc[2]).is_some(),
        "audio does not move to a MIDI track"
    );
    // Moved down a scene (the slot there taken over).
    op(
        &mut s,
        LauncherOp::MoveClip {
            from: SlotKey {
                track: drums,
                scene: sc[2],
            },
            to: SlotKey {
                track: drums,
                scene: sc[0],
            },
            copy: false,
        },
    );
    assert!(s.project().launcher.clip(drums, sc[2]).is_none());
    assert_eq!(s.project().launcher.slots.len(), 5);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().launcher.slots.len(), 6);
    op(&mut s, LauncherOp::AddScene { after: None });
    let sc = scenes(&s);
    op(
        &mut s,
        LauncherOp::CreateClip {
            track: chords,
            scene: sc[3],
        },
    );
    let made = s.project().launcher.clip(chords, sc[3]).unwrap();
    assert!(matches!(
        s.project().clips[&made].content,
        ClipContent::Midi(_)
    ));
    assert!(
        s.dispatch(Action::Launcher(LauncherOp::CreateClip {
            track: drums,
            scene: sc[3],
        }))
        .is_err()
    );
    op(&mut s, LauncherOp::SetQuantize(LaunchQuantize::Bars(2)));
    // Saved and read back.
    let text = faderframe_project::file::to_string(s.project(), None).unwrap();
    let mut loaded = faderframe_project::file::from_str(&text).unwrap().project;
    assert_eq!(loaded.launcher, s.project().launcher);
    // Repair keeps valid slots (and their clips out of the tracks' lists).
    loaded.repair();
    assert_eq!(loaded.launcher, s.project().launcher);
    assert_eq!(loaded.clips_of(drums).len(), drum_clips);
    // A scene goes with its clips; undo brings them back.
    let slots = s.project().launcher.slots.len();
    op(&mut s, LauncherOp::RemoveScene(sc[1]));
    assert_eq!(scenes(&s).len(), 3);
    assert_eq!(s.project().launcher.slots.len(), slots - 2);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(scenes(&s), sc);
    assert_eq!(s.project().launcher.slots.len(), slots);
    // A removed track takes its launcher clips; undo restores them.
    let held: Vec<_> = s
        .project()
        .launcher
        .slots
        .iter()
        .filter(|(k, _)| k.track == drums)
        .map(|(k, c)| (*k, *c))
        .collect();
    assert!(!held.is_empty());
    s.dispatch(Action::Edit(Command::RemoveTrack { track: drums }))
        .unwrap();
    assert!(s.project().launcher.slots.keys().all(|k| k.track != drums));
    assert!(held.iter().all(|(_, c)| !s.project().clips.contains_key(c)));
    s.dispatch(Action::Undo).unwrap();
    for (k, c) in &held {
        assert_eq!(s.project().launcher.slots.get(k), Some(c));
        assert!(s.project().clips.contains_key(c));
    }
    assert_eq!(arranged(&s, drums), drum_clips);
}

#[test]
fn launched_clips_play_and_are_recorded_into_the_arrangement() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let drums = track(&s, "Drums");
    let first = s.project().clips_of(drums)[0].id;
    op(&mut s, LauncherOp::SendClips(vec![first]));
    let scene = scenes(&s)[0];
    let slot = s.project().launcher.clip(drums, scene).unwrap();
    op(&mut s, LauncherOp::SetQuantize(LaunchQuantize::None));
    op(&mut s, LauncherOp::SetRecord(true));
    // Somewhere in the song, so the recording lands away from the start.
    s.dispatch(Action::Transport(TransportAction::Locate(
        faderframe_timeline::MusicalTime::from_quarters(16.0),
    )))
    .unwrap();
    run(&mut s, 50);
    let before: Vec<_> = s.project().clips_of(drums).iter().map(|c| c.id).collect();
    // Launching while stopped starts playback.
    op(
        &mut s,
        LauncherOp::Launch {
            track: drums,
            scene,
        },
    );
    run(&mut s, 400);
    assert!(s.transport().playing);
    let state = *s.launch_state(drums).unwrap();
    let key = SlotKey {
        track: drums,
        scene,
    };
    assert_eq!(state.playing.map(|p| p.0), Some(key.hash()));
    assert!(!state.arrangement);
    assert!(s.launch_progress(drums).is_some());
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, 100);
    // Written into the arrangement from where the launch started: a copy of
    // the launched clip, cut where playback stopped.
    let now = s.project().clips_of(drums);
    let added: Vec<_> = now.iter().filter(|c| !before.contains(&c.id)).collect();
    assert!(!added.is_empty(), "the run was recorded");
    let q = added[0].start.quarters();
    assert!((q - 16.0).abs() < 0.05, "starts where it was launched: {q}");
    let launched = &s.project().clips[&slot];
    match (&added[0].content, &launched.content) {
        (ClipContent::Audio(a), ClipContent::Audio(b)) => {
            assert_eq!(a.source, b.source);
            assert!(a.length < b.length, "cut where it stopped");
        }
        _ => panic!("audio"),
    }
    assert!(s.history_steps().0.iter().any(|l| l == "Record Launches"));
    s.dispatch(Action::Undo).unwrap();
    let ids: Vec<_> = s.project().clips_of(drums).iter().map(|c| c.id).collect();
    assert_eq!(ids, before);
    // Back to the arrangement.
    op(&mut s, LauncherOp::BackToArrangement);
    run(&mut s, 50);
    assert!(s.launch_state(drums).unwrap().arrangement);
}

/// A session with an armed audio track on the dummy device's 1 kHz tone
/// and an armed instrument track on the built-in keyboard.
fn recording_session() -> (Session, TrackId, TrackId, SceneId) {
    use faderframe_project::{InputRouting, Project, TrackKind};
    let mut s = Session::new(Project::new("Rec", 48_000), None, EngineConfig::default()).unwrap();
    let audio = s.add_track(TrackKind::Audio).unwrap();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    for (t, input) in [
        (audio, InputRouting::Hardware { first_channel: 0 }),
        (
            keys,
            InputRouting::Midi {
                port: None,
                channel: None,
            },
        ),
    ] {
        s.edit(Command::SetTrackInput { track: t, input }).unwrap();
        s.edit(Command::SetTrackRecordArm { track: t, on: true })
            .unwrap();
    }
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(1_000.0))],
        &AudioPreferences {
            sample_rate: Some(48_000),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    op(&mut s, LauncherOp::AddScene { after: None });
    op(&mut s, LauncherOp::SetQuantize(LaunchQuantize::Beat));
    let scene = scenes(&s)[0];
    (s, audio, keys, scene)
}

fn wait_for(s: &mut Session, what: &str, done: impl Fn(&Session) -> bool) {
    let start = Instant::now();
    while !done(s) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

#[test]
fn audio_recorded_into_a_slot_becomes_its_clip_and_plays_on_in_time() {
    let (mut s, track, _, scene) = recording_session();
    let arranged = s.project().clips_of(track).len();
    // Stopped: recording starts at once (and so does playback).
    op(&mut s, LauncherOp::Record { track, scene });
    assert_eq!(s.launcher_recording(), Some((track, scene, false)));
    run(&mut s, 700);
    assert!(s.transport().playing);
    // Again: it ends on the next beat (a beat is 24 000 frames at 120 BPM).
    op(&mut s, LauncherOp::Record { track, scene });
    assert_eq!(s.launcher_recording(), Some((track, scene, true)));
    wait_for(&mut s, "the slot's clip", |s| {
        s.project().launcher.clip(track, scene).is_some()
    });
    let clip = s.project().clips[&s.project().launcher.clip(track, scene).unwrap()].clone();
    let ClipContent::Audio(a) = &clip.content else {
        panic!("audio");
    };
    assert_eq!(a.length % 24_000, 0, "whole beats: {}", a.length);
    assert!(a.length >= 24_000);
    // The take has the tone.
    let path = match &s.project().sources[&a.source].spec {
        faderframe_project::SourceSpec::File { path, .. } => path.clone(),
        _ => panic!("a file"),
    };
    let f = faderframe_audio_files::wavstream::WavFile::open(&path).unwrap();
    let mut l = vec![0.0f32; 4_800];
    f.read(a.source_offset as u64, &mut [&mut l], &mut Vec::new())
        .unwrap();
    let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.1, "the tone was recorded: {peak}");
    // It plays on from where the recording started; the arrangement is
    // as it was.
    wait_for(&mut s, "the clip playing", |s| {
        s.launch_state(track).is_some_and(|l| l.playing.is_some())
    });
    let key = SlotKey { track, scene };
    assert_eq!(
        s.launch_state(track).unwrap().playing,
        Some((key.hash(), 0))
    );
    assert_eq!(s.project().clips_of(track).len(), arranged);
    assert_eq!(s.launcher_recording(), None);
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().launcher.clip(track, scene).is_none());
}

#[test]
fn notes_recorded_into_a_slot_make_a_midi_clip_of_whole_beats() {
    let (mut s, audio, keys, scene) = recording_session();
    s.edit(Command::SetTrackRecordArm {
        track: audio,
        on: false,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 100);
    op(&mut s, LauncherOp::Record { track: keys, scene });
    // Waits for the next beat, then takes a note.
    wait_for(&mut s, "the recording to start", |s| {
        s.engine().transport_snapshot().position >= 24_000 + 2_400
    });
    s.midi_keyboard().send(&[0x90, 64, 100]);
    run(&mut s, 150);
    s.midi_keyboard().send(&[0x80, 64, 0]);
    run(&mut s, 50);
    op(&mut s, LauncherOp::Record { track: keys, scene });
    wait_for(&mut s, "the slot's clip", |s| {
        s.project().launcher.clip(keys, scene).is_some()
    });
    let clip = &s.project().clips[&s.project().launcher.clip(keys, scene).unwrap()];
    let ClipContent::Midi(m) = &clip.content else {
        panic!("MIDI");
    };
    assert_eq!(m.notes.len(), 1, "{:?}", m.notes);
    assert_eq!(m.notes[0].key, 64);
    // Whole beats from the beat it started on.
    let beats = m.length.quarters();
    assert!(
        (beats - beats.round()).abs() < 1e-6 && beats >= 1.0,
        "{beats}"
    );
    assert!(m.notes[0].start.quarters() < beats);
}

#[test]
fn follow_actions_go_with_their_clips() {
    use faderframe_project::launcher::{FollowAction, FollowKind};
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let first = s.project().clips_of(drums)[0].id;
    op(&mut s, LauncherOp::SendClips(vec![first]));
    op(&mut s, LauncherOp::AddScene { after: None });
    let sc = scenes(&s);
    let follow = FollowAction {
        kind: FollowKind::Other,
        bars: 2,
        other: Some(FollowKind::Jump(0)),
        chance: 70,
    };
    op(
        &mut s,
        LauncherOp::SetFollow {
            track: drums,
            scene: sc[0],
            follow: Some(follow),
        },
    );
    let key = |scene| SlotKey {
        track: drums,
        scene,
    };
    assert_eq!(s.project().launcher.follow.get(&key(sc[0])), Some(&follow));
    // Moved with the clip.
    op(
        &mut s,
        LauncherOp::MoveClip {
            from: key(sc[0]),
            to: key(sc[1]),
            copy: false,
        },
    );
    assert_eq!(s.project().launcher.follow.get(&key(sc[1])), Some(&follow));
    assert!(!s.project().launcher.follow.contains_key(&key(sc[0])));
    // Gone with it, back with undo.
    op(
        &mut s,
        LauncherOp::ClearSlot {
            track: drums,
            scene: sc[1],
        },
    );
    assert!(s.project().launcher.follow.is_empty());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().launcher.follow.get(&key(sc[1])), Some(&follow));
}

/// Launch modes: a toggle stops on its second press, a gate stops when
/// let go; the settings are saved, go with their clips and undo.
#[test]
fn launch_modes_answer_presses_and_releases() {
    use faderframe_project::launcher::{ClipLaunch, LaunchMode};
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let drums = track(&s, "Drums");
    let first = s.project().clips_of(drums)[0].id;
    op(&mut s, LauncherOp::SendClips(vec![first]));
    op(&mut s, LauncherOp::AddScene { after: None });
    let sc = scenes(&s);
    let key = |scene| SlotKey {
        track: drums,
        scene,
    };
    let set = |s: &mut Session, mode, scene| {
        op(
            s,
            LauncherOp::SetClipLaunch {
                track: drums,
                scene,
                launch: Some(ClipLaunch {
                    mode,
                    quantize: Some(LaunchQuantize::None),
                    legato: false,
                    tempo: None,
                }),
            },
        );
    };
    set(&mut s, LaunchMode::Toggle, sc[0]);
    assert_eq!(
        s.project().launcher.quantize_of(key(sc[0])),
        LaunchQuantize::None
    );
    let slot = key(sc[0]).hash();
    let playing = |s: &Session| s.launch_state(drums).and_then(|t| t.playing).map(|p| p.0);
    let press = |s: &mut Session| {
        op(
            s,
            LauncherOp::Launch {
                track: drums,
                scene: sc[0],
            },
        )
    };
    press(&mut s);
    wait_for(&mut s, "the toggle to play", |s| playing(s) == Some(slot));
    press(&mut s);
    wait_for(&mut s, "the toggle to stop", |s| playing(s).is_none());
    // A gate plays while held.
    set(&mut s, LaunchMode::Gate, sc[0]);
    press(&mut s);
    wait_for(&mut s, "the gate to play", |s| playing(s) == Some(slot));
    run(&mut s, 50);
    assert_eq!(playing(&s), Some(slot), "still held");
    op(
        &mut s,
        LauncherOp::Release {
            track: drums,
            scene: sc[0],
        },
    );
    wait_for(&mut s, "the gate to stop", |s| playing(s).is_none());
    // Moved with the clip, gone with it, back with undo; saved.
    op(
        &mut s,
        LauncherOp::MoveClip {
            from: key(sc[0]),
            to: key(sc[1]),
            copy: false,
        },
    );
    assert_eq!(
        s.project().launcher.launch_of(key(sc[1])).mode,
        LaunchMode::Gate
    );
    assert!(!s.project().launcher.launch.contains_key(&key(sc[0])));
    let json = serde_json::to_string(&s.project().launcher).unwrap();
    let back: faderframe_project::launcher::Launcher = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s.project().launcher);
    op(
        &mut s,
        LauncherOp::ClearSlot {
            track: drums,
            scene: sc[1],
        },
    );
    assert!(s.project().launcher.launch.is_empty());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().launcher.launch_of(key(sc[1])).mode,
        LaunchMode::Gate
    );
}

/// A fixed length ends a slot recording by itself (a punch range does not
/// limit it); recording on a MIDI clip that plays overdubs: the notes
/// join its loop where they fell, in one undo step.
#[test]
fn slot_recordings_have_a_fixed_length_and_midi_clips_take_overdubs() {
    let (mut s, audio, keys, scene) = recording_session();
    s.edit(Command::SetTrackRecordArm {
        track: audio,
        on: false,
    })
    .unwrap();
    // A punch range somewhere else is not the slot's business.
    s.edit(Command::SetPunch {
        range: faderframe_project::MusicalRange::new(
            faderframe_timeline::MusicalTime::from_quarters(64.0),
            faderframe_timeline::MusicalTime::from_quarters(68.0),
        ),
        enabled: true,
    })
    .unwrap();
    op(
        &mut s,
        LauncherOp::SetRecordOptions {
            bars: 1,
            count_in: 0,
        },
    );
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 100);
    op(&mut s, LauncherOp::Record { track: keys, scene });
    wait_for(&mut s, "the recording to start", |s| {
        s.engine().transport_snapshot().position >= 24_000 + 2_400
    });
    s.midi_keyboard().send(&[0x90, 60, 100]);
    run(&mut s, 100);
    s.midi_keyboard().send(&[0x80, 60, 0]);
    // Nobody ends it: a bar (two seconds) later it is the slot's clip.
    wait_for(&mut s, "the fixed-length clip", |s| {
        s.project().launcher.clip(keys, scene).is_some()
    });
    let id = s.project().launcher.clip(keys, scene).unwrap();
    let length = s.project().clips[&id].as_midi().unwrap().length;
    assert!((length.quarters() - 4.0).abs() < 1e-6, "a bar: {length:?}");
    assert_eq!(s.project().clips[&id].as_midi().unwrap().notes.len(), 1);
    // Overdub (until ended): it plays on, and a note played now joins it.
    op(
        &mut s,
        LauncherOp::SetRecordOptions {
            bars: 0,
            count_in: 0,
        },
    );
    wait_for(&mut s, "the clip playing", |s| {
        s.launch_state(keys).is_some_and(|l| l.playing.is_some())
    });
    op(&mut s, LauncherOp::Record { track: keys, scene });
    assert_eq!(s.launcher_recording(), Some((keys, scene, false)));
    run(&mut s, 700);
    s.midi_keyboard().send(&[0x90, 67, 100]);
    run(&mut s, 100);
    s.midi_keyboard().send(&[0x80, 67, 0]);
    run(&mut s, 50);
    op(&mut s, LauncherOp::Record { track: keys, scene });
    wait_for(&mut s, "the overdub", |s| {
        s.project().clips[&id].as_midi().unwrap().notes.len() == 2
    });
    let notes = &s.project().clips[&id].as_midi().unwrap().notes;
    let new = notes.iter().find(|n| n.key == 67).unwrap();
    assert!(
        new.start.quarters() < 4.0,
        "inside the loop: {:?}",
        new.start
    );
    assert!(
        s.launch_state(keys).is_some_and(|l| l.playing.is_some()),
        "still playing"
    );
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Overdub")
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips[&id].as_midi().unwrap().notes.len(), 1);
}

/// Record to Arrangement writes each loop once it has played (while still
/// playing), all of it one undo step, and writes the mixer's moves as
/// automation (a lane made for the fader).
#[test]
fn recording_to_the_arrangement_writes_as_it_plays_with_the_mixer() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    // A bar is a second.
    s.edit(Command::SetTempo { bpm: 240.0 }).unwrap();
    let lead = track(&s, "Lead Synth");
    op(&mut s, LauncherOp::AddScene { after: None });
    let scene = scenes(&s)[0];
    op(&mut s, LauncherOp::CreateClip { track: lead, scene });
    op(&mut s, LauncherOp::SetQuantize(LaunchQuantize::None));
    op(&mut s, LauncherOp::SetRecord(true));
    assert!(
        s.project()
            .track(lead)
            .unwrap()
            .automation
            .lane(faderframe_automation::AutomationTarget::TrackVolume)
            .is_none()
    );
    let before = s.project().clips_of(lead).len();
    op(&mut s, LauncherOp::Launch { track: lead, scene });
    // The fader moves while it plays.
    run(&mut s, 300);
    s.dispatch(Action::BeginGesture("Fader".into())).unwrap();
    for db in [-3.0f32, -6.0, -9.0] {
        s.dispatch(Action::Edit(Command::SetTrackVolume { track: lead, db }))
            .unwrap();
        run(&mut s, 80);
    }
    s.dispatch(Action::EndGesture).unwrap();
    // Two loops played: in the arrangement already.
    wait_for(&mut s, "loops written while playing", |s| {
        s.project().clips_of(lead).len() >= before + 2
    });
    assert!(s.transport().playing);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, 100);
    let labels = s.history_steps().0;
    assert_eq!(
        labels.iter().filter(|l| *l == "Record Launches").count(),
        1,
        "{labels:?}"
    );
    let lane = s
        .project()
        .track(lead)
        .unwrap()
        .automation
        .lane(faderframe_automation::AutomationTarget::TrackVolume)
        .cloned()
        .expect("a lane for the fader's moves");
    assert!(lane.curve.points().len() >= 2, "{:?}", lane.curve.points());
    // Undoing the recording takes every loop back.
    while s.history_steps().0.last().map(String::as_str) != Some("Record Launches") {
        s.dispatch(Action::Undo).unwrap();
    }
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips_of(lead).len(), before);
}
