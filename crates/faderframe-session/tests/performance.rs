#![allow(clippy::unwrap_used)]
//! Performance statistics from a running (dummy) stream: totals, tracks and
//! plugin instances add up, peaks bound averages, reset clears.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::builtin;
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn run_for(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
    }
}

#[test]
fn loads_are_reported_per_plugin_track_and_in_total() {
    let mut s = Session::new(Project::new("Perf", 48_000), None, EngineConfig::default()).unwrap();
    let busy = s.add_track(TrackKind::Audio).unwrap();
    let plain = s.add_track(TrackKind::Audio).unwrap();
    for i in 0..3 {
        s.dispatch(Action::InsertPlugin {
            track: busy,
            index: i,
            plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
        })
        .unwrap();
    }
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    // Per-track and per-plugin timing runs while someone reads the figures.
    let _ = s.performance();
    s.tick(0.0);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run_for(&mut s, Duration::from_millis(1200));
    s.poll_performance();

    let r = s.performance().clone();
    assert!(r.running);
    assert!(r.buffer_size > 0 && r.sample_rate > 0);
    assert!(r.callbacks > 0);
    assert!(r.total.average > 0.0, "{:?}", r.total);
    assert!(r.total.peak >= r.total.average * 0.5);
    assert!(r.history.len() >= 3, "{}", r.history.len());
    assert!(r.graph_nodes > 0);

    let t_busy = r.tracks.iter().find(|t| t.track == busy).unwrap();
    let t_plain = r.tracks.iter().find(|t| t.track == plain).unwrap();
    assert_eq!(t_busy.plugins.len(), 3);
    assert!(t_plain.plugins.is_empty());
    let plugins: f64 = t_busy.plugins.iter().map(|p| p.load.average).sum();
    for p in &t_busy.plugins {
        assert!(p.load.average > 0.0, "{p:?}");
        assert!(!p.failed && !p.bypassed);
        assert_eq!(p.name, "Echo");
    }
    // A track's load includes its plugins and its own mixing work.
    assert!(
        t_busy.load.average + 1e-9 >= plugins,
        "{} < {plugins}",
        t_busy.load.average
    );
    assert!(t_busy.mixing.average > 0.0);
    // The graph is part of the callback.
    let tracks: f64 = r.tracks.iter().map(|t| t.load.average).sum();
    assert!(
        (tracks - r.graph.average).abs() < 1e-6,
        "{tracks} vs {}",
        r.graph.average
    );
    assert!(r.graph.average <= r.total.average * 1.05 + 1e-4);
    assert_eq!(r.plugins_by_load().len(), 3);

    // Nobody looking: node timing goes off, the total keeps being measured.
    let start = Instant::now();
    while s.engine().node_timing() && start.elapsed() < Duration::from_secs(4) {
        std::thread::sleep(Duration::from_millis(20));
        s.tick(0.02);
    }
    assert!(
        !s.engine().node_timing(),
        "per-node timing switches off when unused"
    );
    assert!(s.dsp_load().average > 0.0);

    s.dispatch(Action::ResetPerformance).unwrap();
    s.poll_performance();
    assert!(s.performance().history.len() <= 1);
    s.stop_audio();
}

#[test]
fn the_meter_opens_in_layouts_saved_before_it_existed() {
    use faderframe_session::WorkspaceAction;
    use faderframe_workspace::ViewId;
    let mut s = Session::new(Project::new("Old", 48_000), None, EngineConfig::default()).unwrap();
    // A layout from before the performance meter: the view is unknown.
    let layout = s.workspace_mut().active_layout_mut();
    layout.remove_view(&ViewId::performance());
    layout.views.remove(&ViewId::performance());
    assert!(!layout.is_showing(&ViewId::performance()));
    s.dispatch(Action::Workspace(WorkspaceAction::ShowView(
        ViewId::performance(),
    )))
    .unwrap();
    assert!(
        s.workspace()
            .active_layout()
            .is_showing(&ViewId::performance())
    );
}
