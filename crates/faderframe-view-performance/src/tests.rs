use super::*;
use faderframe_audio::dummy::DummyBackend;
use faderframe_core::builtin;
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginRef, Project};
use faderframe_session::{AudioPreferences, TransportAction};
use faderframe_ui_canvas::{Modifiers, RecordingPainter};
use std::time::{Duration, Instant};

const SIZE: Size = Size {
    w: 1100.0,
    h: 700.0,
};

fn run(
    view: &mut PerformanceView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn click(pos: Point, button: PointerButton, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

/// A measured session: "Heavy" with two Echos, "Light" without plugins.
fn measured() -> (Session, TrackId, TrackId) {
    let mut s = Session::new(Project::new("Perf", 48_000), None, EngineConfig::default()).unwrap();
    let heavy = s.add_track(TrackKind::Audio).unwrap();
    let light = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: heavy,
        name: "Heavy".into(),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: light,
        name: "Light".into(),
    }))
    .unwrap();
    for i in 0..2 {
        s.dispatch(Action::InsertPlugin {
            track: heavy,
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
    let _ = s.performance();
    s.tick(0.0);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(900) {
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
    }
    s.poll_performance();
    (s, heavy, light)
}

fn row_y(view: &PerformanceView, s: &Session, want: (TrackId, Option<PluginInstanceId>)) -> f32 {
    let report = s.performance();
    let l = view.layout(SIZE);
    view.row_rects(report, l.table)
        .into_iter()
        .find(|(r, _)| match *r {
            Row::Track(ti) => (report.tracks[ti].track, None) == want,
            Row::Plugin(ti, pi) => {
                (
                    report.tracks[ti].track,
                    Some(report.tracks[ti].plugins[pi].plugin),
                ) == want
            }
            Row::Mixing(_) => false,
        })
        .map(|(_, r)| r.center().y)
        .unwrap()
}

#[test]
fn load_formatting_and_bar_scales() {
    assert_eq!(format_load(0.0), "—");
    assert_eq!(format_load(0.0004), "<0.1 %");
    assert_eq!(format_load(0.034), "3.4 %");
    assert_eq!(format_load(0.234), "23 %");
    assert_eq!(bar_scale(0.03), 0.05);
    assert_eq!(bar_scale(0.3), 0.5);
    assert_eq!(bar_scale(0.9), 1.0);
    assert_eq!(bar_scale(1.7), 2.0);
}

#[test]
fn tracks_list_their_plugins_heaviest_first_and_collapse() {
    let (mut s, heavy, light) = measured();
    let mut view = PerformanceView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.balanced_clips());
    let texts = p.texts();
    assert!(texts.contains(&"Heavy") && texts.contains(&"Light"));
    assert!(texts.contains(&"2 plugins"));
    assert!(
        texts.iter().any(|t| t.starts_with("plugins ")),
        "breakdown legend"
    );
    assert!(texts.contains(&"DSP LOAD"));

    let report = s.performance().clone();
    let rows = visible_rows(&view, &report);
    // Tracks heaviest first, each followed by its plugins.
    let load = |t: TrackId| {
        report
            .tracks
            .iter()
            .find(|x| x.track == t)
            .unwrap()
            .load
            .average
    };
    let tracks: Vec<TrackId> = rows.iter().filter(|r| r.1.is_none()).map(|r| r.0).collect();
    assert_eq!(tracks.len(), 3, "master, Heavy, Light");
    assert!(tracks.windows(2).all(|w| load(w[0]) >= load(w[1])));
    let h = rows.iter().position(|r| *r == (heavy, None)).unwrap();
    assert!(rows[h + 1].1.is_some() && rows[h + 2].1.is_some());
    assert!(rows.contains(&(light, None)));

    // Collapse Heavy with its chevron.
    let y = row_y(&view, &s, (heavy, None));
    run(
        &mut view,
        click(Point::new(PAD + 6.0, y), PointerButton::Primary, 1),
        &s,
    );
    assert!(
        visible_rows(&view, s.performance())
            .iter()
            .all(|r| r.1.is_none()),
        "no plugin rows once collapsed"
    );
    // Clicking a row selects its track.
    let y = row_y(&view, &s, (light, None));
    let (a, _) = run(
        &mut view,
        click(Point::new(400.0, y), PointerButton::Primary, 1),
        &s,
    );
    assert_eq!(
        a,
        vec![Action::SelectTracks {
            tracks: vec![light],
            mode: SelectMode::Replace
        }]
    );
    // Sort by name: Heavy, Light (alphabetical); twice: project order.
    let l = view.layout(SIZE);
    let header_name = Point::new(PAD + 20.0, l.header.center().y);
    run(&mut view, click(header_name, PointerButton::Primary, 1), &s);
    assert_eq!(view.sort(), Sort::Name);
    run(&mut view, click(header_name, PointerButton::Primary, 1), &s);
    assert_eq!(view.sort(), Sort::Order);
    s.stop_audio();
}

#[test]
fn plugins_mode_lists_every_instance_with_actions() {
    let (mut s, heavy, _) = measured();
    let mut view = PerformanceView::new(Theme::default());
    let l = view.layout(SIZE);
    let (modes, reset) = view.toolbar_parts(l.toolbar);
    run(
        &mut view,
        click(modes[1].1.center(), PointerButton::Primary, 1),
        &s,
    );
    assert_eq!(view.mode(), Mode::Plugins);
    let rows = visible_rows(&view, s.performance());
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(t, p)| *t == heavy && p.is_some()));

    // Double-click: the plugin's editor; right-click: a menu.
    let first = rows[0];
    let y = row_y(&view, &s, first);
    let (a, _) = run(
        &mut view,
        click(Point::new(300.0, y), PointerButton::Primary, 2),
        &s,
    );
    assert_eq!(
        a,
        vec![Action::OpenPluginEditor {
            track: heavy,
            plugin: first.1.unwrap(),
            generic: false
        }]
    );
    let (_, req) = run(
        &mut view,
        click(Point::new(300.0, y), PointerButton::Secondary, 1),
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req.first() else {
        panic!("expected a plugin menu");
    };
    assert!(items.iter().any(|i| i.label == "Show Editor"));
    assert!(items.iter().any(|i| i.label == "Bypass"));

    // Reset peaks.
    let (a, _) = run(
        &mut view,
        click(reset.center(), PointerButton::Primary, 1),
        &s,
    );
    assert_eq!(a, vec![Action::ResetPerformance]);

    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().iter().any(|t| t.starts_with("Echo — ")));
    s.stop_audio();
}

#[test]
fn stopped_audio_is_explained() {
    let s = Session::new(Project::new("Idle", 48_000), None, EngineConfig::default()).unwrap();
    let mut view = PerformanceView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().iter().any(|t| t.starts_with("Audio is stopped")));
    assert!(!view.wants_frames(&s));
}
