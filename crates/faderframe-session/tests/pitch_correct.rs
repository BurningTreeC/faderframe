//! Pitch Correct in a session: an insert on a vocal track with its
//! latency compensated, a MIDI track can play it (its target notes), and
//! its Range changes the latency (the graph is built again).
#![allow(clippy::unwrap_used)]

use faderframe_core::{ParameterId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::pitch_correct::{self as pc, id};
use faderframe_project::{Command, OutputRouting, PluginRef, Project, TrackKind};
use faderframe_session::{Action, Session};

const SR: u32 = 48_000;

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!(
        "timed out waiting for {what}: {}",
        s.engine().graph_latency()
    );
}

fn new_track(s: &mut Session, kind: TrackKind) -> TrackId {
    let before: Vec<TrackId> = s.project().tracks.iter().map(|t| t.id).collect();
    s.dispatch(Action::AddTrack(kind)).unwrap();
    s.project()
        .tracks
        .iter()
        .find(|t| !before.contains(&t.id))
        .unwrap()
        .id
}

#[test]
fn a_midi_track_plays_pitch_correct_on_a_vocal_track() {
    let mut s = Session::new(Project::new("Vocals", SR), None, EngineConfig::default()).unwrap();
    let vocal = new_track(&mut s, TrackKind::Audio);
    let midi = new_track(&mut s, TrackKind::Midi);
    let name = s.project().track(vocal).unwrap().name.clone();
    // Nothing on the vocal takes notes yet: not a choice.
    let labels = |s: &Session| {
        s.midi_instrument_choices(midi)
            .into_iter()
            .map(|c| c.label)
            .collect::<Vec<_>>()
    };
    assert!(!labels(&s).iter().any(|l| l.starts_with(&name)));
    s.dispatch(Action::InsertPlugin {
        track: vocal,
        index: 0,
        plugin: PluginRef::builtin(builtin::PITCH_CORRECT, "FaderFrame Pitch Correct"),
    })
    .unwrap();
    let plugin = s.project().track(vocal).unwrap().inserts[0].id;
    assert!(s.plugin_tap(plugin).is_some(), "hosted");
    let choices = s.midi_instrument_choices(midi);
    let plays = choices
        .iter()
        .find(|c| c.label == format!("{name} (Pitch Correct)"))
        .unwrap();
    s.dispatch(plays.action.clone()).unwrap();
    assert_eq!(
        s.project().track(midi).unwrap().output,
        OutputRouting::Track { track: vocal }
    );
    // Its latency is the graph's.
    let params = faderframe_plugin_host::ParamValues::new(pc::parameters());
    let rate = f64::from(SR);
    let want = pc::latency(&params, rate);
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == want,
        "Pitch Correct's latency",
    );
    // Soprano's range is shorter: the graph follows.
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: vocal,
        plugin,
        parameter: ParameterId(id::RANGE),
        value: Some(0.0),
    }))
    .unwrap();
    params.set_by_id(ParameterId(id::RANGE), 0.0).unwrap();
    let shorter = pc::latency(&params, rate);
    assert!(shorter < want);
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == shorter,
        "the shorter latency",
    );
}
