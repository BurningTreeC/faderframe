#![allow(clippy::unwrap_used)]
//! Recording edge cases from the arranger audit (§2.9), with the dummy
//! device's test tone: several armed tracks recording a new track per
//! pass, a frozen armed track, the count-in after the audio device
//! reopens, Flatten Comp's declicks, and a realistic sequence — a take
//! over a loop with pre-roll, comped, split, moved with snapping, undone
//! to the start, saved and opened again.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    Clip, ClipContent, Command, CompSegment, InputRouting, MusicalRange, Project, Take, TakeFolder,
    TrackKind,
};
use faderframe_session::editing::EditMode;
use faderframe_session::{
    Action, AudioPreferences, LoopRecordMode, SelectMode, Session, TransportAction,
};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn prefs() -> AudioPreferences {
    AudioPreferences {
        sample_rate: Some(SR),
        buffer_size: Some(64),
        ..Default::default()
    }
}

/// `n` armed mono audio tracks on input 1, the dummy device running.
fn session(n: usize) -> (Session, Vec<TrackId>) {
    let mut s = Session::new(Project::new("Rec", SR), None, EngineConfig::default()).unwrap();
    let mut tracks = Vec::new();
    for _ in 0..n {
        let t = s.add_track(TrackKind::Audio).unwrap();
        s.edit(Command::SetTrackInput {
            track: t,
            input: InputRouting::Hardware { first_channel: 0 },
        })
        .unwrap();
        s.edit(Command::SetTrackRecordArm { track: t, on: true })
            .unwrap();
        tracks.push(t);
    }
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(1_000.0))],
        &prefs(),
    )
    .unwrap();
    (s, tracks)
}

fn secs(s: &Session, seconds: f64) -> MusicalTime {
    s.engine()
        .samples_to_musical(s.project(), (seconds * SR as f64) as i64)
}

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.tick(0.005);
        if s.transport().playing && done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

fn wait_position(s: &mut Session, seconds: f64) {
    let target = (seconds * SR as f64) as i64;
    wait_until(s, |s| s.transport().position >= target, "position");
}

fn wait_wraps(s: &mut Session, n: usize) {
    let mut last = s.transport().position;
    let mut wraps = 0;
    wait_until(
        s,
        |s| {
            let pos = s.transport().position;
            if pos < last {
                wraps += 1;
            }
            last = pos;
            wraps >= n
        },
        "loop wraps",
    );
}

fn clips(s: &Session, t: TrackId) -> Vec<Clip> {
    let mut v: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    v.sort_by_key(|c| c.start);
    v
}

fn set_loop(s: &mut Session, seconds: f64) {
    let range = MusicalRange::new(MusicalTime::ZERO, secs(s, seconds)).unwrap();
    s.dispatch(Action::Transport(TransportAction::SetLoop(Some(range))))
        .unwrap();
}

/// Two armed tracks, a new track per pass: each track's passes go right
/// under it (they used to land at indexes from before the first track's
/// passes went in, B's among A's).
#[test]
fn each_armed_track_gets_its_pass_tracks_right_under_it() {
    let (mut s, t) = session(2);
    let mut r = s.record;
    r.loop_mode = LoopRecordMode::NewTracks;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    set_loop(&mut s, 0.25);
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_wraps(&mut s, 2);
    wait_position(&mut s, 0.2);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let names: Vec<String> = s
        .project()
        .tracks
        .iter()
        .filter(|x| x.kind == TrackKind::Audio)
        .map(|x| x.name.clone())
        .collect();
    let name = |id: TrackId| s.project().track(id).unwrap().name.clone();
    let (a, b) = (name(t[0]), name(t[1]));
    let ia = names.iter().position(|n| *n == a).unwrap();
    let ib = names.iter().position(|n| *n == b).unwrap();
    let a_passes: Vec<usize> = (0..names.len())
        .filter(|i| names[*i].starts_with(&format!("{a} Pass")))
        .collect();
    let b_passes: Vec<usize> = (0..names.len())
        .filter(|i| names[*i].starts_with(&format!("{b} Pass")))
        .collect();
    assert!(
        a_passes.len() >= 2 && b_passes.len() == a_passes.len(),
        "{names:?}"
    );
    assert_eq!(
        a_passes,
        ((ia + 1)..(ia + 1 + a_passes.len())).collect::<Vec<_>>(),
        "{names:?}"
    );
    assert_eq!(ib, ia + 1 + a_passes.len(), "{names:?}");
    assert_eq!(
        b_passes,
        ((ib + 1)..(ib + 1 + b_passes.len())).collect::<Vec<_>>(),
        "{names:?}"
    );
    // One undo step takes every pass track away.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .tracks
            .iter()
            .filter(|x| x.kind == TrackKind::Audio)
            .count(),
        2
    );
}

/// An armed track that is frozen does not record (it cannot take clips),
/// and the other armed tracks' takes are kept — before, the frozen track
/// refused the whole "Record" step and every take was lost.
#[test]
fn a_frozen_armed_track_is_left_out_and_the_others_record() {
    let (mut s, t) = session(2);
    // Something to freeze on the second track.
    let source = AudioSourceId(9_100);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(faderframe_project::AudioSource {
            id: source,
            name: "tone".into(),
            spec: faderframe_project::SourceSpec::Generated {
                generator: faderframe_audio_files::GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 2,
                    seed: 3,
                },
            },
        }),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(9_200),
            track: t[1],
            name: "loop".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(faderframe_project::AudioClip {
                source,
                source_offset: 0,
                length: SR as i64,
                gain_db: 0.0,
                fades: Default::default(),
                stretch: Default::default(),
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
            }),
        }),
    }))
    .unwrap();
    s.dispatch(Action::FreezeTrack(t[1])).unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    while !s.bouncing().is_empty() {
        assert!(Instant::now() < end, "freeze timed out");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.project().track(t[1]).unwrap().freeze.is_some());
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_position(&mut s, 0.3);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    assert_eq!(clips(&s, t[0]).len(), 1, "the take of the unfrozen track");
    assert_eq!(clips(&s, t[1]).len(), 1, "the frozen track as it was");
    assert_eq!(clips(&s, t[1])[0].id, ClipId(9_200));
    assert!(
        s.notices()
            .any(|n| n.text.contains("frozen and not recorded")),
        "the user is told"
    );
}

/// The count-in's click setting is part of the record settings: it
/// survives the audio device reopening (which makes a new engine), and
/// holds only while counting in is on.
#[test]
fn the_count_in_survives_the_audio_device_reopening() {
    let (mut s, _) = session(1);
    let mut r = s.record;
    assert!(r.count_in_click_only, "on by default");
    assert!(!s.engine().metronome().count_in_only(), "not counting in");
    r.count_in = true;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    assert!(s.engine().metronome().count_in_only());
    s.stop_audio();
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(1_000.0))],
        &prefs(),
    )
    .unwrap();
    assert!(s.engine().metronome().count_in_only(), "the new engine too");
    r.count_in_click_only = false;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    assert!(!s.engine().metronome().count_in_only());
}

/// Flatten Comp plays what the comp played: a comp edge inside the folder
/// that touches no other piece keeps its short declick.
#[test]
fn flatten_keeps_the_declicks_of_a_comp_with_gaps() {
    let mut s = Session::new(Project::new("Flat", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(9_300);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(faderframe_project::AudioSource {
            id: source,
            name: "loop".into(),
            spec: faderframe_project::SourceSpec::Generated {
                generator: faderframe_audio_files::GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 4,
                    seed: 1,
                },
            },
        }),
    }))
    .unwrap();
    let len = 3 * SR as i64;
    let mut f = TakeFolder::new(len);
    for name in ["A", "B"] {
        f.add_take(Take {
            name: name.into(),
            source,
            source_offset: 0,
            start: 0,
            end: len,
            gain_db: 0.0,
            rating: 0,
        });
    }
    // A for a second, silence, B from two seconds on.
    f.comp = vec![
        CompSegment {
            start: 0,
            take: Some(0),
        },
        CompSegment {
            start: SR as i64,
            take: None,
        },
        CompSegment {
            start: 2 * SR as i64,
            take: Some(1),
        },
    ];
    let folder = ClipId(9_400);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: folder,
            track: t,
            name: "takes".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Takes(f),
        }),
    }))
    .unwrap();
    s.dispatch(Action::FlattenTakes(folder)).unwrap();
    let c = clips(&s, t);
    assert_eq!(c.len(), 2);
    let a = c[0].as_audio().unwrap();
    let b = c[1].as_audio().unwrap();
    assert_eq!(a.fades.fade_in, 0, "the folder's own edge");
    assert_eq!(a.fades.fade_out, faderframe_project::COMP_DECLICK);
    assert_eq!(b.fades.fade_in, faderframe_project::COMP_DECLICK);
    assert_eq!(b.fades.fade_out, 0);
}

/// A realistic sequence: a take recorded over a loop with a pre-roll,
/// recorded again (a take folder), comped, split, moved with snapping —
/// then everything undone, redone, saved, opened again and compared.
#[test]
fn record_comp_split_move_undo_save_and_reopen() {
    let (mut s, t) = session(1);
    let t = t[0];
    let mut r = s.record;
    r.preroll = faderframe_session::record::Roll {
        amount: 0.5,
        unit: faderframe_session::record::RollUnit::Seconds,
    };
    r.loop_mode = LoopRecordMode::LastPass;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    // A loop from 0.5 s to 1 s; recording starts at its start.
    let range = MusicalRange::new(secs(&s, 0.5), secs(&s, 1.0)).unwrap();
    s.dispatch(Action::Transport(TransportAction::SetLoop(Some(range))))
        .unwrap();
    let record_pass = |s: &mut Session| {
        s.dispatch(Action::Transport(TransportAction::Locate(range.start)))
            .unwrap();
        // Record and Play pressed together (a tape-style surface): Record
        // starts playing with the pre-roll; Play, before the engine reports
        // playing, must not restart the take from the pre-roll's start.
        s.dispatch(Action::Transport(TransportAction::ToggleRecord))
            .unwrap();
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        wait_wraps(s, 1);
        wait_position(s, 0.9);
        s.dispatch(Action::Transport(TransportAction::Stop))
            .unwrap();
        s.wait_for_recordings();
    };
    let steps0 = s.history_steps().0.len();
    record_pass(&mut s);
    let first = clips(&s, t);
    assert_eq!(first.len(), 1, "one take");
    // Placed at the loop's start (not at the pre-roll's).
    assert_eq!(first[0].start, range.start);
    record_pass(&mut s);
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    let folder = c[0].id;
    let f = c[0].as_takes().expect("a take folder").clone();
    assert_eq!(f.takes.len(), 2);
    // Comp: the first take in the folder's first half.
    let mut comped = f.clone();
    comped.set_comp(0, f.length / 2, Some(0));
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip: folder,
        start: c[0].start,
        content: Box::new(ClipContent::Takes(comped)),
    }))
    .unwrap();
    // Split at the loop's middle, keep both pieces selected.
    s.dispatch(Action::SelectClips {
        clips: vec![folder],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Locate(secs(&s, 0.75))))
        .unwrap();
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(clips(&s, t).len(), 2);
    assert_eq!(s.selection.clips.len(), 2, "both pieces stay selected");
    // Move the right piece onto the second's grid line (Grid mode).
    s.dispatch(Action::SetEditMode(EditMode::Grid)).unwrap();
    let right = clips(&s, t)[1].clone();
    s.dispatch(Action::BeginGesture("Move Clip".into()))
        .unwrap();
    s.dispatch(Action::MoveClips {
        clips: vec![right.id],
        by: MusicalTime::from_quarters(2.0).ticks(),
        tracks: 0,
    })
    .unwrap();
    s.dispatch(Action::EndGesture).unwrap();
    let moved = clips(&s, t);
    assert_eq!(
        moved[1].start,
        right.start + MusicalTime::from_quarters(2.0)
    );
    let edited = s.project().clone();
    // Everything undone: back to an empty track; redone: as edited.
    let steps = s.history_steps().0.len() - steps0;
    for _ in 0..steps {
        s.dispatch(Action::Undo).unwrap();
    }
    assert!(clips(&s, t).is_empty(), "undone to the start");
    for _ in 0..steps {
        s.dispatch(Action::Redo).unwrap();
    }
    assert_eq!(clips(&s, t), clips_of(&edited, t));
    // Saved and opened again: the same clips, folders and comps.
    let dir = std::env::temp_dir().join(format!("ff-audit-reopen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("take.ffproj");
    s.save_as(&path).unwrap();
    s.stop_audio();
    let mut reopened =
        Session::new(Project::new("Other", SR), None, EngineConfig::default()).unwrap();
    reopened.open(&path).unwrap();
    assert_eq!(clips(&reopened, t), clips_of(&edited, t));
    let _ = std::fs::remove_dir_all(&dir);
}

fn clips_of(p: &Project, t: TrackId) -> Vec<Clip> {
    let mut v: Vec<_> = p.clips_of(t).into_iter().cloned().collect();
    v.sort_by_key(|c| c.start);
    v
}

/// Loop recording inside a punch range with latency: every pass is the
/// punch range less the latency — none ends with the next pass's first
/// frames (audio from the next punch-in, a click where the comp plays it).
#[test]
fn passes_inside_a_punch_range_end_with_their_own_audio() {
    let (mut s, t) = session(1);
    let t = t[0];
    let mut r = s.record;
    r.loop_mode = LoopRecordMode::Takes;
    r.latency_offset = 480;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    set_loop(&mut s, 0.5);
    let punch = MusicalRange::new(secs(&s, 0.1), secs(&s, 0.3)).unwrap();
    s.dispatch(Action::Transport(TransportAction::SetPunch(Some(punch))))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::TogglePunch))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_wraps(&mut s, 2);
    wait_position(&mut s, 0.4);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    let f = c[0].as_takes().expect("a folder of passes");
    assert!(f.takes.len() >= 3, "{} passes", f.takes.len());
    let window = (0.2 * SR as f64) as i64;
    for take in &f.takes {
        assert_eq!(take.end - take.start, window - 480, "{}", take.name);
    }
}

/// Keep Last Pass with Stop pressed just after a wrap: the last complete
/// pass is kept, not the moment of the pass that had only begun.
#[test]
fn keep_last_pass_keeps_the_last_complete_pass() {
    let (mut s, t) = session(1);
    let t = t[0];
    let mut r = s.record;
    r.loop_mode = LoopRecordMode::LastPass;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    set_loop(&mut s, 0.25);
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_wraps(&mut s, 2);
    wait_position(&mut s, 0.03);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    let a = c[0].as_audio().expect("one clip of one pass");
    assert_eq!(a.length, (0.25 * SR as f64) as i64, "a whole pass");
    assert_eq!(c[0].start, MusicalTime::ZERO);
}
