#![allow(clippy::unwrap_used)]
//! Automation in the session: parameter list, lanes, display values and
//! writing in Touch and Latch mode while playing.

use faderframe_audio::dummy::DummyBackend;
use faderframe_automation::{AutomationCurve, AutomationPoint, CurveShape};
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef, Project, SendTap, TrackKind};
use faderframe_session::{
    Action, AudioPreferences, AutomationMode, AutomationTarget, ParamKind, Session, TransportAction,
};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn session() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Auto", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    (s, t)
}

fn lane_points(s: &Session, t: TrackId, target: AutomationTarget) -> Vec<AutomationPoint> {
    s.project()
        .track(t)
        .unwrap()
        .automation
        .lane(target)
        .unwrap()
        .curve
        .points()
        .to_vec()
}

#[test]
fn every_parameter_is_listed_with_a_usable_range() {
    let (mut s, t) = session();
    let aux = s.add_track(TrackKind::Aux).unwrap();
    s.dispatch(Action::AddSend {
        track: t,
        target: aux,
        level_db: -6.0,
        tap: SendTap::PostFader,
    })
    .unwrap();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
    })
    .unwrap();
    let params = s.automatable_parameters(t);
    let names: Vec<_> = params.iter().map(|p| p.name.clone()).collect();
    for want in [
        "Volume",
        "Pan",
        "Mute",
        "Echo: Time",
        "Echo: Feedback",
        "Echo: Bypass",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} missing from {names:?}"
        );
    }
    assert!(names.iter().any(|n| n.starts_with("Send → ")));
    let vol = &params[0];
    assert_eq!(vol.kind, ParamKind::Gain);
    for db in [-30.0, -6.0, 0.0, 6.0] {
        assert!(
            (vol.from_normal(vol.to_normal(db)) - db).abs() < 0.05,
            "{db}"
        );
    }
    let time = params.iter().find(|p| p.name == "Echo: Time").unwrap();
    assert_eq!(time.kind, ParamKind::Log);
    assert!((time.from_normal(time.to_normal(250.0)) - 250.0).abs() < 1e-6);
}

#[test]
fn lanes_show_hide_and_drive_the_displayed_value() {
    let (mut s, t) = session();
    s.dispatch(Action::ShowAutomation {
        track: t,
        target: AutomationTarget::TrackVolume,
    })
    .unwrap();
    let lanes = s.shown_lanes(t);
    assert_eq!(lanes.len(), 1);
    let mut lane = lanes[0].clone();
    assert!(
        !s.is_automated(t, AutomationTarget::TrackVolume),
        "empty lanes do nothing"
    );
    lane.curve = AutomationCurve::from_points(vec![
        AutomationPoint {
            time: MusicalTime::ZERO,
            value: -12.0,
            shape: CurveShape::Linear,
        },
        AutomationPoint {
            time: MusicalTime::from_quarters(4.0),
            value: 0.0,
            shape: CurveShape::Linear,
        },
    ]);
    s.edit(Command::SetAutomationLane {
        track: t,
        lane: Box::new(lane.clone()),
    })
    .unwrap();
    assert!(s.is_automated(t, AutomationTarget::TrackVolume));
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(2.0),
    )))
    .unwrap();
    assert!((s.display_value(t, AutomationTarget::TrackVolume).unwrap() - -6.0).abs() < 1e-6);
    s.dispatch(Action::SetAutomationMode {
        track: t,
        lane: lane.id,
        mode: AutomationMode::Off,
    })
    .unwrap();
    assert_eq!(
        s.display_value(t, AutomationTarget::TrackVolume),
        Some(0.0),
        "static value when off"
    );
    s.dispatch(Action::HideAutomationLane(lane.id)).unwrap();
    assert!(s.shown_lanes(t).is_empty());
    // The header button brings back lanes with points.
    s.dispatch(Action::ToggleTrackAutomation(t)).unwrap();
    assert_eq!(s.shown_lanes(t).len(), 1);
}

fn play_for(s: &mut Session, ms: u64, mut each: impl FnMut(&mut Session, f32)) {
    // The first step at 0 (sleeping may overshoot on busy machines).
    each(s, 0.0);
    let start = Instant::now();
    let total = Duration::from_millis(ms);
    while start.elapsed() < total {
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
        let f = start.elapsed().as_secs_f32() / total.as_secs_f32();
        each(s, f.min(1.0));
    }
}

#[test]
fn touch_and_latch_write_automation_while_playing() {
    let (mut s, t) = session();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::ShowAutomation {
        track: t,
        target: AutomationTarget::TrackVolume,
    })
    .unwrap();
    let lane = s.shown_lanes(t)[0].id;
    s.dispatch(Action::SetAutomationMode {
        track: t,
        lane,
        mode: AutomationMode::Touch,
    })
    .unwrap();

    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    play_for(&mut s, 150, |_, _| {});
    // A fader gesture: ramp from 0 dB down to -24 dB.
    s.dispatch(Action::BeginGesture("Volume".into())).unwrap();
    play_for(&mut s, 400, |s, f| {
        s.dispatch(Action::Edit(Command::SetTrackVolume {
            track: t,
            db: -24.0 * f,
        }))
        .unwrap();
    });
    assert!(s.is_lane_writing(lane));
    s.dispatch(Action::EndGesture).unwrap();
    assert!(!s.is_lane_writing(lane), "touch ends with the gesture");
    let pts = lane_points(&s, t, AutomationTarget::TrackVolume);
    assert!(
        pts.len() >= 2 && pts.len() < 20,
        "thinned: {} points",
        pts.len()
    );
    assert!((pts.first().unwrap().value - 0.0).abs() < 2.0);
    assert!((pts.last().unwrap().value - -24.0).abs() < 1.0);
    assert!(pts.windows(2).all(|w| w[0].time <= w[1].time));

    // Latch: keeps writing the last value until playback stops.
    s.dispatch(Action::SetAutomationMode {
        track: t,
        lane,
        mode: AutomationMode::Latch,
    })
    .unwrap();
    s.dispatch(Action::BeginGesture("Volume".into())).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackVolume { track: t, db: -3.0 }))
        .unwrap();
    s.dispatch(Action::EndGesture).unwrap();
    assert!(s.is_lane_writing(lane), "latch continues after release");
    play_for(&mut s, 200, |_, _| {});
    let stop_at = s.playhead();
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    assert!(!s.is_lane_writing(lane));
    let pts = lane_points(&s, t, AutomationTarget::TrackVolume);
    let last = pts.last().unwrap();
    assert!((last.value - -3.0).abs() < 1e-6);
    assert!(
        last.time >= stop_at - MusicalTime::from_quarters(0.25),
        "held until stop"
    );

    // One undo removes the latched pass, the next the touch pass' result.
    let before = pts.len();
    s.dispatch(Action::Undo).unwrap();
    assert!(lane_points(&s, t, AutomationTarget::TrackVolume).len() < before);
}
