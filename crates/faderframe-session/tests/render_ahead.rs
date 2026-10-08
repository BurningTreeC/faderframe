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
    // Whether the device keeps the wall clock's pace (a busy runner's
    // dummy device falls behind and catches up in bursts).
    let callbacks = |s: &Session| s.stream_status().map_or(0, |st| st.callbacks);
    let paced_from = (std::time::Instant::now(), callbacks(&s));
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
    let st = s.stream_status().unwrap();
    let expected = paced_from.0.elapsed().as_secs_f64() * f64::from(st.sample_rate)
        / f64::from(st.buffer_size.max(1));
    let got = (callbacks(&s) - paced_from.1) as f64;
    if (got - expected).abs() > 2.0 + expected * 0.05 {
        eprintln!(
            "the device did not keep pace ({got} callbacks for {expected:.1}): late {late} not checked"
        );
    } else {
        assert!(late <= 4, "late {late} times");
    }
    s.stop_audio();
}

/// A locate while stopped shows at once and stays shown: render-ahead holds
/// it back until the new position is primed, and the playhead used to fall
/// back to where it was until then (the session took the engine's old
/// position on the next tick).
#[test]
fn a_locate_shows_at_once_while_render_ahead_primes_it() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_render_ahead(Some(Duration::from_millis(200)))
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    run(&mut s, Duration::from_millis(300));
    assert!(!s.engine().ahead_tracks().is_empty(), "rendered ahead");
    let bar3 = faderframe_timeline::MusicalTime::from_quarters_i(8);
    let target = s.engine().musical_to_samples(s.project(), bar3);
    s.dispatch(Action::Transport(TransportAction::Locate(bar3)))
        .unwrap();
    // Every tick until the engine has it, the playhead is there.
    let end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < end {
        s.tick(0.016);
        assert_eq!(s.transport().position, target, "the playhead stays put");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        s.engine().transport_snapshot().position,
        target,
        "and the engine got there"
    );
    s.stop_audio();
}
