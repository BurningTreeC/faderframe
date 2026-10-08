//! The setlist and show mode: songs from sections, each stopped on its
//! exact last frame, the next one stood on, played after its gap, or
//! played straight on.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_project::setlist::AfterSong;
use faderframe_session::setlist::{SetlistOp, ShowOp};
use faderframe_session::{Action, AudioPreferences, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

/// 240 BPM: a bar (four quarters) a second. Songs A (bar 1), B (bar 3)
/// and C (bar 4, right after B).
fn session() -> Session {
    let mut p = Project::new("Show", SR);
    p.timeline.tempo.set_initial_bpm(240.0);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    for (a, b) in [(0.0, 4.0), (8.0, 12.0), (12.0, 16.0)] {
        s.dispatch(Action::AddSection {
            start: q(a),
            end: q(b),
        })
        .unwrap();
    }
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    s
}

fn wait(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_show_plays_its_songs_and_stops_on_their_last_frames() {
    let mut s = session();
    s.dispatch(Action::Setlist(SetlistOp::FromSections))
        .unwrap();
    assert_eq!(s.setlist().songs.len(), 3);
    s.dispatch(Action::Setlist(SetlistOp::Then(
        0,
        AfterSong::Next { gap: 0.2 },
    )))
    .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::Then(1, AfterSong::Continue)))
        .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::Rename(2, "Finale".into())))
        .unwrap();
    s.dispatch(Action::Show(ShowOp::Enter)).unwrap();
    assert!(s.show_mode());
    assert_eq!(s.show().current, 0);
    s.dispatch(Action::Show(ShowOp::PlayStop)).unwrap();
    wait(&mut s, |s| s.transport().playing, "playing");
    // Song A: stopped on its last frame (one second), then B stood on.
    wait(
        &mut s,
        |s| !s.transport().playing && s.transport().position == i64::from(SR),
        "the stop at A's end",
    );
    wait(&mut s, |s| s.show().current == 1, "B stood on");
    assert!(s.show_countdown().is_some(), "B waits for its gap");
    // B after its gap, from its start; it plays on into C.
    wait(&mut s, |s| s.transport().playing, "B playing");
    wait(
        &mut s,
        |s| s.show().current == 2 && s.transport().playing,
        "C playing on",
    );
    // C (the last): stopped on its last frame.
    wait(
        &mut s,
        |s| !s.transport().playing && s.transport().position == 4 * i64::from(SR),
        "the stop at C's end",
    );
    // Leaving the show: nothing stops anything any more.
    s.dispatch(Action::Show(ShowOp::Leave)).unwrap();
    assert!(!s.show_mode());
}

#[test]
fn the_setlist_edits_are_undone_one_by_one() {
    let mut s = session();
    s.dispatch(Action::Setlist(SetlistOp::FromSections))
        .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::Move { from: 2, to: 0 }))
        .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::Notes(0, "Capo 2".into())))
        .unwrap();
    assert_eq!(s.setlist().songs[0].start, q(12.0));
    assert_eq!(s.setlist().songs[0].notes, "Capo 2");
    s.dispatch(Action::Undo).unwrap();
    assert!(s.setlist().songs[0].notes.is_empty());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.setlist().songs[0].start, q(0.0));
    s.dispatch(Action::Undo).unwrap();
    assert!(s.setlist().songs.is_empty());
    // An empty setlist has no show.
    assert!(s.dispatch(Action::Show(ShowOp::Enter)).is_err());
}
