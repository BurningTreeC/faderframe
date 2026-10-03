//! The Tools view's meters follow the played audio of the chosen track.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_session::analysis::LevelScale;
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn play_for(s: &mut Session, secs: f32) {
    let end = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(16));
    }
}

#[test]
fn master_and_track_analysis_while_playing() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(48_000),
            buffer_size: Some(256),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.analysis_source(), s.project().master_id());
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    play_for(&mut s, 1.2);
    let l = s.analyzer().loudness.read();
    assert!(l.momentary > -40.0 && l.momentary < 0.0, "{}", l.momentary);
    assert!(l.true_peak > -40.0, "{}", l.true_peak);
    let [left, right] = s.analysis_levels();
    assert!(left.peak > -40.0 && right.peak > -40.0);
    assert!(s.analyzer().phase.correlation() > 0.0);
    let (hz, db) = s.analyzer().spectrum.loudest();
    assert!(hz > 20.0 && db > -60.0, "{hz} Hz {db} dB");

    // A silent track: nothing but the floor.
    let pad = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Pad")
        .unwrap()
        .id;
    s.dispatch(Action::SetAnalysisSource(Some(pad))).unwrap();
    s.dispatch(Action::SetLevelScale(LevelScale::K(14)))
        .unwrap();
    assert_eq!(s.analysis_settings().scale.zero_db(), -14.0);
    play_for(&mut s, 0.6);
    assert_eq!(s.analysis_source(), Some(pad));
    // The pad enters at bar 5: still silent here.
    assert!(s.analyzer().loudness.read().momentary < -60.0);
    s.dispatch(Action::ResetAnalysis).unwrap();
    assert_eq!(s.analyzer().loudness.read().integrated, f64::NEG_INFINITY);
    s.stop_audio();
}
