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
