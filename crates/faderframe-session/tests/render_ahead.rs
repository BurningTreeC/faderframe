//! Render-ahead in a running session: tracks nobody plays live are
//! rendered ahead and sound; arming one moves it to the audio thread at
//! once, disarming brings it back once playback stops.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::demo::demo_project;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

/// The demo's Lead Synth and master alone: what is rendered ahead stays
/// light enough for a slow CI machine's debug build to keep ahead.
fn lead_alone() -> Project {
    let mut p = demo_project(EngineConfig::default().sample_rate);
    p.tracks
        .retain(|t| t.name == "Lead Synth" || t.kind == TrackKind::Master);
    let kept: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
    p.clips.retain(|_, c| kept.contains(&c.track));
    for t in &mut p.tracks {
        t.sends.clear();
    }
    p
}

#[test]
fn tracks_render_ahead_until_armed() {
    let mut s = Session::new(lead_alone(), None, EngineConfig::default()).unwrap();
    s.set_render_ahead(Some(Duration::from_millis(150)))
        .unwrap();
    let synth = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .id;
    let ahead = |s: &Session| s.engine().ahead_tracks().contains(&synth);
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
    assert!(ahead(&s), "the instrument track");
    let others = s.render_ahead_status().0 - 1;
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
    assert!(!ahead(&s));
    assert_eq!(s.render_ahead_status().0, others);
    assert!(level(&s) > -50.0, "still heard: {}", level(&s));
    // Disarmed while playing: stays there until playback stops.
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: synth,
        on: false,
    }))
    .unwrap();
    run(&mut s, Duration::from_millis(200));
    assert!(!ahead(&s));
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, Duration::from_millis(300));
    assert!(ahead(&s), "back after the stop");
    assert_eq!(s.render_ahead_status().0, others + 1);
    // A stale or mis-keyed ring misses every block (~60 here); a busy
    // machine's dummy device, catching up after oversleeping, consumes
    // blocks faster than real time and may outrun the anticipator by one
    // or two — a real device never does.
    let late = s.render_ahead_status().1;
    assert!(late <= 4, "late {late} times");
    s.stop_audio();
}
