use super::*;
use faderframe_automation::AutomationTarget;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::RecordingPainter;

const SIZE: Size = Size::new(1300.0, 360.0);

fn session() -> (Session, TrackId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    s.dispatch(Action::ShowAutomation {
        track: drums,
        target: AutomationTarget::TrackVolume,
    })
    .unwrap();
    (s, drums)
}

/// Feed an event; apply what the view emits to the session.
fn run(view: &mut AutomationView, s: &mut Session, ev: ViewEvent) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    requests
}

fn down(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

fn up(pos: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

fn volume_lane(s: &Session, t: TrackId) -> AutomationLane {
    s.project()
        .track(t)
        .unwrap()
        .automation
        .lane(AutomationTarget::TrackVolume)
        .unwrap()
        .clone()
}

#[test]
fn lists_every_track_with_its_lanes() {
    let (mut s, drums) = session();
    let mut view = AutomationView::new(Theme::default());
    let rows = view.rows(&s);
    let lane = volume_lane(&s, drums).id;
    let i = rows.iter().position(|r| *r == Row::Track(drums)).unwrap();
    assert_eq!(rows[i + 1], Row::Lane(drums, lane));
    assert!(rows.len() > 6, "every automatable track is listed");
    // Only the selected tracks.
    s.selection.tracks.clear();
    s.selection.tracks.insert(drums);
    view.selected_only = true;
    assert_eq!(
        view.rows(&s),
        vec![Row::Track(drums), Row::Lane(drums, lane)]
    );
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
}

#[test]
fn clicks_add_drag_and_delete_points() {
    let (mut s, drums) = session();
    let mut view = AutomationView::new(Theme::default());
    let plot = view.layout(SIZE).plot;
    // Click in the editor: a point at the pointer (and the static value at
    // the start), then dragged up.
    let at = Point::new(plot.x + plot.w * 0.5, plot.bottom() - plot.h * 0.25);
    run(&mut view, &mut s, down(at, 1));
    let dragged = Point::new(at.x, plot.y + plot.h * 0.2);
    run(
        &mut view,
        &mut s,
        ViewEvent::PointerMove {
            pos: dragged,
            modifiers: Modifiers::NONE,
            dragging: true,
        },
    );
    run(&mut view, &mut s, up(dragged));
    let lane = volume_lane(&s, drums);
    assert_eq!(lane.curve.points().len(), 2);
    let p = lane.curve.points()[1];
    let param = s
        .automation_param(drums, AutomationTarget::TrackVolume)
        .unwrap();
    assert!((param.to_normal(p.value) - 0.8).abs() < 0.02, "dragged up");
    // One undo step for add + drag.
    s.dispatch(Action::Undo).unwrap();
    assert!(volume_lane(&s, drums).curve.is_empty());
    s.dispatch(Action::Redo).unwrap();
    // Double-click deletes it.
    let x = view.x_of(&s, plot, p.time);
    let y = AutomationView::value_y(&param, plot, p.value);
    run(&mut view, &mut s, down(Point::new(x, y), 2));
    assert_eq!(volume_lane(&s, drums).curve.points().len(), 1);
}

#[test]
fn lane_tools_write_thin_clear_and_modes() {
    let (mut s, drums) = session();
    let mut view = AutomationView::new(Theme::default());
    let lane = volume_lane(&s, drums);
    // Write Value at the playhead: the fader's −0 dB… wherever it is.
    let written = AutomationView::write_value(&s, drums, &lane).unwrap();
    assert_eq!(written.curve.points().len(), 1);
    let fader = s
        .static_value(drums, AutomationTarget::TrackVolume)
        .unwrap();
    assert_eq!(written.curve.points()[0].value, fader);
    // Over a selection: the value inside, jumps at the edges.
    let mut ramp = lane.clone();
    for (q, v) in [(0.0, -20.0), (16.0, 0.0)] {
        ramp.curve.insert(AutomationPoint {
            time: MusicalTime::from_quarters(q),
            value: v,
            shape: CurveShape::Linear,
        });
    }
    s.dispatch(AutomationView::edit(drums, ramp.clone()))
        .unwrap();
    s.selection.range = Some(faderframe_session::EditRange::new(
        MusicalTime::from_quarters(4.0),
        MusicalTime::from_quarters(8.0),
    ));
    let filled = AutomationView::write_value(&s, drums, &volume_lane(&s, drums)).unwrap();
    let mid = filled
        .curve
        .value_at(MusicalTime::from_quarters(6.0))
        .unwrap();
    assert_eq!(mid, fader);
    let outside = filled
        .curve
        .value_at(MusicalTime::from_quarters(12.0))
        .unwrap();
    assert!(
        (outside - -5.0).abs() < 1e-6,
        "untouched outside: {outside}"
    );
    // Thin: a straight line needs only its ends.
    let mut line = lane.clone();
    for i in 0..=10 {
        line.curve.insert(AutomationPoint {
            time: MusicalTime::from_quarters(i as f64),
            value: -10.0,
            shape: CurveShape::Linear,
        });
    }
    let thinned = AutomationView::thinned(&s, drums, &line).unwrap();
    assert_eq!(thinned.curve.points().len(), 2);
    // Buttons: Clear, then All Off / All Read for every listed lane.
    view.select(drums, lane.id);
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.press_button(Button::Clear, &s, &mut cx);
    view.press_button(Button::AllOff, &s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let l = volume_lane(&s, drums);
    assert!(l.curve.is_empty());
    assert_eq!(l.mode, AutomationMode::Off);
}

#[test]
fn the_plus_offers_the_missing_parameters() {
    let (mut s, drums) = session();
    let mut view = AutomationView::new(Theme::default());
    let l = view.layout(SIZE);
    let rows = view.rows(&s);
    let i = rows.iter().position(|r| *r == Row::Track(drums)).unwrap();
    let add = AutomationView::row_parts(view.row_rect(l.list, i), Row::Track(drums))[0].1;
    let req = run(&mut view, &mut s, down(add.center(), 1));
    let HostRequest::ContextMenu { items, .. } = &req[0] else {
        panic!("a menu")
    };
    assert!(items.iter().any(|m| m.label == "Pan"));
    assert!(
        !items.iter().any(|m| m.label == "Volume"),
        "already has a lane"
    );
}
