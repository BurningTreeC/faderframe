#![allow(clippy::unwrap_used)]
//! Where Stop leaves the playhead: back where playback started, where it
//! stopped (Insertion Follows Playback), or at the start of the project
//! (Stop Returns to Start). With the dummy device, in real time.

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::{Action, AudioPreferences, EditFlag, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

/// Play from quarter 4 for a while, stop; the engine's position then.
fn play_and_stop(s: &mut Session) -> i64 {
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters_i(4),
    )))
    .unwrap();
    run(s, Duration::from_millis(50));
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(s, Duration::from_millis(300));
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(s, Duration::from_millis(100));
    s.processing_position()
}

#[test]
fn stop_returns_to_the_play_start_stays_or_goes_to_the_start() {
    let mut s = Session::new(Project::new("Stop", 48_000), None, EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    // 120 bpm: quarter 4 is two seconds in.
    let from = 96_000;
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        false,
    ))
    .unwrap();
    assert_eq!(play_and_stop(&mut s), from, "back where playback started");
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        true,
    ))
    .unwrap();
    assert!(play_and_stop(&mut s) > from, "where it stopped");
    s.dispatch(Action::SetEditFlag(EditFlag::StopToStart, true))
        .unwrap();
    assert_eq!(play_and_stop(&mut s), 0, "the start of the project");
    // Either way round of the other flag.
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        false,
    ))
    .unwrap();
    assert_eq!(play_and_stop(&mut s), 0);
}

fn running(project: Project) -> Session {
    let mut s = Session::new(project, None, EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s
}

fn transport(s: &mut Session, a: TransportAction) {
    s.dispatch(Action::Transport(a)).unwrap();
}

/// Play from `from` quarters for `ms`, stop; where playback ran from (its
/// lowest position while playing) and where Stop left it.
fn play_from(s: &mut Session, from: i64, ms: u64) -> (i64, i64) {
    transport(
        s,
        TransportAction::Locate(MusicalTime::from_quarters_i(from)),
    );
    run(s, Duration::from_millis(50));
    transport(s, TransportAction::Play);
    let mut lowest = i64::MAX;
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
        if s.engine().transport_snapshot().playing {
            lowest = lowest.min(s.processing_position());
        }
    }
    transport(s, TransportAction::Stop);
    run(s, Duration::from_millis(100));
    (lowest, s.processing_position())
}

/// The Play Start Marker: playback starts there whatever the playhead
/// (while it is on), Return to Start on Stop comes back to it; off, the
/// playhead counts again. Undoable, saved with the project.
#[test]
fn the_play_start_marker_is_where_play_starts_and_stop_returns_to_it() {
    let mut s = running(Project::new("Marker", 48_000));
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        false,
    ))
    .unwrap();
    // Quarter 8: four seconds in.
    transport(
        &mut s,
        TransportAction::SetPlayStart(Some(MusicalTime::from_quarters_i(8))),
    );
    let marker = 192_000;
    let (from, stopped) = play_from(&mut s, 2, 300);
    assert!(
        (marker..marker + 4_800).contains(&from),
        "from the marker: {from}"
    );
    assert_eq!(stopped, marker, "back to where playback started");
    // Off: from the playhead again (the marker stays).
    transport(&mut s, TransportAction::TogglePlayStart);
    assert!(!s.project().play_start.unwrap().on);
    let (from, stopped) = play_from(&mut s, 2, 300);
    assert!(from < 48_000 + 4_800, "from the playhead: {from}");
    assert_eq!(stopped, 48_000);
    // Undo: on again.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().play_start.unwrap().on);
    // Saved and opened again.
    let dir = std::env::temp_dir().join(format!("ff-play-start-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("marker.ffproj");
    s.save_as(&path).unwrap();
    let mut t = Session::new(Project::new("Other", 48_000), None, EngineConfig::default()).unwrap();
    t.open(&path).unwrap();
    assert_eq!(
        t.project().play_start,
        Some(faderframe_project::PlayStart::at(
            MusicalTime::from_quarters_i(8)
        ))
    );
    // Removed.
    transport(&mut s, TransportAction::SetPlayStart(None));
    assert_eq!(s.project().play_start, None);
    let (from, _) = play_from(&mut s, 2, 200);
    assert!(from < 48_000 + 4_800, "from the playhead: {from}");
    drop(t);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Return to Start on Stop through loop passes: back to where playback
/// started (inside the loop), not to the loop's start.
#[test]
fn return_to_start_on_stop_comes_back_through_loop_passes() {
    let mut s = running(Project::new("Loop", 48_000));
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        false,
    ))
    .unwrap();
    // A loop of two seconds (quarters 0–4), playing from quarter 1.
    transport(
        &mut s,
        TransportAction::SetLoop(faderframe_project::MusicalRange::new(
            MusicalTime::ZERO,
            MusicalTime::from_quarters_i(4),
        )),
    );
    let (_, stopped) = play_from(&mut s, 1, 2_600);
    assert_eq!(stopped, 24_000);
}

/// After a take with a pre-roll, Return to Start on Stop comes back to
/// where the take began (the playhead before the pre-roll), so the next
/// take pre-rolls the same way — not to the pre-roll's start, which
/// moved every take one pre-roll earlier.
#[test]
fn return_to_start_after_a_pre_roll_comes_back_to_the_record_start() {
    use faderframe_project::{Command, InputRouting, TrackKind};
    let mut s = Session::new(Project::new("Pre", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Hardware { first_channel: 0 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(440.0))],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        false,
    ))
    .unwrap();
    let mut r = s.record;
    r.preroll = faderframe_session::Roll::beats(2.0);
    r.preroll_on_bar = false;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    for _ in 0..2 {
        // The playhead at quarter 4 (2 s): the pre-roll plays from 1 s.
        transport(
            &mut s,
            TransportAction::Locate(MusicalTime::from_quarters_i(4)),
        );
        run(&mut s, Duration::from_millis(50));
        transport(&mut s, TransportAction::ToggleRecord);
        run(&mut s, Duration::from_millis(1_400));
        transport(&mut s, TransportAction::Stop);
        run(&mut s, Duration::from_millis(100));
        s.wait_for_recordings();
        assert_eq!(s.processing_position(), 96_000, "back at the record start");
    }
    let starts: Vec<_> = s.project().clips_of(t).iter().map(|c| c.start).collect();
    assert!(
        !starts.is_empty() && starts.iter().all(|c| *c == MusicalTime::from_quarters_i(4)),
        "both takes from quarter 4: {starts:?}"
    );
}
