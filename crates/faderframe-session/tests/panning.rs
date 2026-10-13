#![allow(clippy::unwrap_used)]
//! Pan modes in the session: what can be automated per mode, writing dual
//! pan, MIDI learn, selected tracks following, the menu's choices and track
//! presets.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{ChannelLayout, DualPan, PanMode, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::preset::TrackPreset;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::{
    Action, AudioPreferences, AutomationMode, AutomationTarget, SelectMode, Session,
    TransportAction,
};
use std::time::{Duration, Instant};

fn session() -> Session {
    Session::new(Project::new("Pan", 48_000), None, EngineConfig::default()).unwrap()
}

fn stereo_track(s: &mut Session) -> TrackId {
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Stereo,
    }))
    .unwrap();
    t
}

fn names(s: &Session, t: TrackId) -> Vec<String> {
    s.automatable_parameters(t)
        .into_iter()
        .map(|p| p.name)
        .collect()
}

fn set_mode(s: &mut Session, track: TrackId, mode: PanMode) {
    s.dispatch(Action::Edit(Command::SetTrackPanMode { track, mode }))
        .unwrap();
}

#[test]
fn each_mode_offers_its_own_controls() {
    let mut s = session();
    let mono = s.add_track(TrackKind::Audio).unwrap();
    let stereo = stereo_track(&mut s);
    assert!(names(&s, mono).contains(&"Pan".to_string()));
    assert!(s.pan_mode_choices(mono).is_empty(), "mono has one pan");
    assert!(names(&s, stereo).contains(&"Balance".to_string()));
    let choices = s.pan_mode_choices(stereo);
    let labels: Vec<_> = choices
        .iter()
        .map(|c| (c.label.as_str(), c.checked))
        .collect();
    assert_eq!(labels, [("Balance", true), ("Dual Pan", false)]);

    s.dispatch(choices[1].action.clone()).unwrap();
    assert_eq!(s.project().track(stereo).unwrap().pan_mode, PanMode::Dual);
    let n = names(&s, stereo);
    assert!(n.contains(&"Left Pan".to_string()) && n.contains(&"Right Pan".to_string()));
    assert!(!n.contains(&"Balance".to_string()));
    let left = s
        .automation_param(stereo, AutomationTarget::DualPanLeft)
        .unwrap();
    assert_eq!((left.min, left.max, left.default), (-1.0, 1.0, -1.0));

    // A mono destination: nothing to pan, no mode to choose.
    let bus = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: bus,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: stereo,
        output: faderframe_project::OutputRouting::Track { track: bus },
    }))
    .unwrap();
    assert!(s.pan_mode_choices(stereo).is_empty());

    // One undo step per edit (routing, bus format, bus, the switch).
    for _ in 0..3 {
        s.dispatch(Action::Undo).unwrap();
    }
    assert_eq!(s.project().track(stereo).unwrap().pan_mode, PanMode::Dual);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().track(stereo).unwrap().pan_mode,
        PanMode::Balance
    );
}

#[test]
fn midi_learn_moves_one_side_of_a_dual_pan() {
    let mut s = session();
    let t = stereo_track(&mut s);
    set_mode(&mut s, t, PanMode::Dual);
    match s.command_for(t, AutomationTarget::DualPanRight, 0.25) {
        Some(Command::SetTrackDualPan { pan, .. }) => {
            assert_eq!(
                pan,
                DualPan {
                    left: -1.0,
                    right: 0.25
                }
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn selected_stereo_tracks_switch_and_move_together() {
    let mut s = session();
    let (a, b) = (stereo_track(&mut s), stereo_track(&mut s));
    let mono = s.add_track(TrackKind::Audio).unwrap();
    let select = |s: &mut Session, tracks: Vec<TrackId>| {
        s.dispatch(Action::SelectTracks {
            tracks,
            mode: SelectMode::Replace,
        })
        .unwrap();
    };
    select(&mut s, vec![a, b, mono]);
    set_mode(&mut s, a, PanMode::Dual);
    assert_eq!(s.project().track(b).unwrap().pan_mode, PanMode::Dual);
    assert_eq!(s.project().track(mono).unwrap().pan_mode, PanMode::Balance);
    // B on its own first, then both: each side moves by as much as the
    // touched track's did, clamped at the ends.
    select(&mut s, vec![b]);
    let dual = |s: &Session, t: TrackId| s.project().track(t).unwrap().dual_pan;
    let set = |s: &mut Session, track: TrackId, left: f32, right: f32| {
        s.dispatch(Action::Edit(Command::SetTrackDualPan {
            track,
            pan: DualPan { left, right },
        }))
        .unwrap();
    };
    set(&mut s, b, -0.8, 0.6);
    assert_eq!(dual(&s, a), DualPan::ORIGINAL);
    select(&mut s, vec![a, b]);
    set(&mut s, a, -0.5, 0.75);
    let moved = dual(&s, b);
    assert!((moved.left - -0.3).abs() < 1e-6 && (moved.right - 0.35).abs() < 1e-6);
    set(&mut s, a, 1.0, 0.75);
    assert_eq!(dual(&s, b).left, 1.0);
}

#[test]
fn track_presets_carry_the_pan_mode() {
    let mut s = session();
    let t = stereo_track(&mut s);
    set_mode(&mut s, t, PanMode::Dual);
    let pan = DualPan {
        left: -0.3,
        right: 0.4,
    };
    s.dispatch(Action::Edit(Command::SetTrackDualPan { track: t, pan }))
        .unwrap();
    let preset = TrackPreset::capture(s.project(), s.project().track(t).unwrap());
    let mut p = Project::new("Other", 48_000);
    let (made, _) = preset.instantiate(&mut p, None);
    assert_eq!((made.pan_mode, made.dual_pan), (PanMode::Dual, pan));
    // An older preset file without them: balance, as it was.
    let mut json: serde_json::Value = serde_json::to_value(&preset).unwrap();
    let obj = json.as_object_mut().unwrap();
    obj.remove("pan_mode");
    obj.remove("dual_pan");
    let old: TrackPreset = serde_json::from_value(json).unwrap();
    assert_eq!(
        (old.pan_mode, old.dual_pan),
        (PanMode::Balance, DualPan::ORIGINAL)
    );
}

fn play_for(s: &mut Session, ms: u64, mut each: impl FnMut(&mut Session, f32)) {
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
fn dual_pan_is_written_side_by_side() {
    let mut s = session();
    let t = stereo_track(&mut s);
    set_mode(&mut s, t, PanMode::Dual);
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    for target in [
        AutomationTarget::DualPanLeft,
        AutomationTarget::DualPanRight,
    ] {
        s.dispatch(Action::ShowAutomation { track: t, target })
            .unwrap();
    }
    for lane in s.shown_lanes(t).iter().map(|l| l.id).collect::<Vec<_>>() {
        s.dispatch(Action::SetAutomationMode {
            track: t,
            lane,
            mode: AutomationMode::Touch,
        })
        .unwrap();
    }
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    play_for(&mut s, 100, |_, _| {});
    // Only the left channel moves: only its lane is written.
    s.dispatch(Action::BeginGesture("Pan".into())).unwrap();
    play_for(&mut s, 300, |s, f| {
        s.dispatch(Action::Edit(Command::SetTrackDualPan {
            track: t,
            pan: DualPan {
                left: -1.0 + f,
                right: 1.0,
            },
        }))
        .unwrap();
    });
    s.dispatch(Action::EndGesture).unwrap();
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    let points = |target| {
        s.project()
            .track(t)
            .unwrap()
            .automation
            .lane(target)
            .map_or(0, |l| l.curve.points().len())
    };
    assert!(points(AutomationTarget::DualPanLeft) >= 2);
    assert_eq!(points(AutomationTarget::DualPanRight), 0);
    let lane = s
        .project()
        .track(t)
        .unwrap()
        .automation
        .lane(AutomationTarget::DualPanLeft)
        .unwrap();
    let last = lane.curve.points().last().unwrap();
    assert!((last.value - 0.0).abs() < 0.05, "{}", last.value);
    // The knob shows the lane where it plays.
    assert!(s.shown_dual_pan(s.project().track(t).unwrap()).left > -0.5);
}
