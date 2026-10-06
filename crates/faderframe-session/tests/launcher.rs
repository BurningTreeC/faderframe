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
