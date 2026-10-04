//! Render-ahead in a running session: tracks nobody plays live are
//! rendered ahead and sound; arming one moves it to the audio thread at
//! once, disarming brings it back once playback stops.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::Command;
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

#[test]
fn tracks_render_ahead_until_armed() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_render_ahead(Some(Duration::from_millis(150)))
        .unwrap();
    let synth = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .id;
    // Nothing selected: the instrument is not live.
    s.dispatch(Action::SelectTracks {
        tracks: Vec::new(),
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    run(&mut s, Duration::from_millis(200));
    assert_eq!(s.render_ahead_status().0, 1, "the instrument track");
    // Into the melody (bar 5), playing.
    s.dispatch(Action::Transport(TransportAction::Locate(
        faderframe_timeline::MusicalTime::from_quarters_i(16),
    )))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(1200));
    let level = |s: &Session| s.meter(synth).left.level_db;
    assert!(level(&s) > -50.0, "rendered ahead and heard: {}", level(&s));
    // Armed: on the audio thread at once, still sounding.
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: synth,
        on: true,
    }))
    .unwrap();
    run(&mut s, Duration::from_millis(300));
    assert_eq!(s.render_ahead_status().0, 0);
    assert!(level(&s) > -50.0, "still heard: {}", level(&s));
    // Disarmed while playing: stays there until playback stops.
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: synth,
        on: false,
    }))
    .unwrap();
    run(&mut s, Duration::from_millis(200));
    assert_eq!(s.render_ahead_status().0, 0);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, Duration::from_millis(300));
    assert_eq!(s.render_ahead_status().0, 1, "back after the stop");
    assert_eq!(s.render_ahead_status().1, 0, "never late");
    s.stop_audio();
}
