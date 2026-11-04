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
    let index = s.project().track(drums).unwrap().inserts.len();
    s.dispatch(Action::InsertPlugin {
        track: drums,
        index,
        plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::EQ, "Test EQ"),
    })
    .unwrap();
    let mut view = AutomationView::new(Theme::default());
    let l = view.layout(SIZE);
    let rows = view.rows(&s);
    let i = rows.iter().position(|r| *r == Row::Track(drums)).unwrap();
    let add = AutomationView::row_parts(view.row_rect(l.list, i), Row::Track(drums))[0].1;
    let req = run(&mut view, &mut s, down(add.center(), 1));
    let HostRequest::ContextMenu { items, .. } = &req[0] else {
        panic!("a menu")
    };
    // The drums are stereo: their pan control is a balance.
    assert!(items.iter().any(|m| m.label == "Balance"));
    assert!(
        !items.iter().any(|m| m.label == "Volume"),
        "already has a lane"
    );
    // The devices' parameters in their submenus, the EQ's by band.
    let eq = items.iter().position(|m| m.label == "Test EQ").unwrap();
    assert!(eq > 0 && items[..eq].iter().any(|m| m.children.is_empty()));
    let gain = faderframe_ui_canvas::menu::find(items, &["Test EQ", "Band 2", "Gain"]).unwrap();
    s.dispatch(gain.action.clone().unwrap()).unwrap();
    let want = s
        .automatable_parameters(drums)
        .into_iter()
        .find(|p| p.name == "Test EQ: Band 2 Gain")
        .unwrap()
        .target;
    let lanes = &s.project().track(drums).unwrap().automation.lanes;
    assert!(lanes.iter().any(|l| l.target == want));
}

mod keyboard {
    use super::*;
    use faderframe_ui_canvas::access::{find_node, focus_order, press_key};
    use faderframe_ui_canvas::{AccessNode, AccessRole, Key};

    fn named(view: &AutomationView, s: &Session, label: &str) -> AccessNode<Action> {
        let nodes = view.accessible(SIZE, s);
        focus_order(&nodes)
            .into_iter()
            .find(|n| n.label.starts_with(label))
            .cloned()
            .unwrap_or_else(|| panic!("no control '{label}'"))
    }

    fn key(
        view: &mut AutomationView,
        s: &mut Session,
        label: &str,
        k: Key,
    ) -> Vec<HostRequest<Action>> {
        let id = named(view, s, label).id;
        let nodes = view.accessible(SIZE, s);
        let (actions, requests) = press_key(view, &nodes, id, k, Modifiers::NONE, SIZE, s)
            .unwrap_or_else(|| panic!("{k:?} does something on '{label}'"));
        for a in actions {
            s.dispatch(a).unwrap();
        }
        requests
    }

    fn menu(requests: Vec<HostRequest<Action>>) -> Vec<MenuItem<Action>> {
        match requests.into_iter().next() {
            Some(HostRequest::ContextMenu { items, .. }) => items,
            _ => panic!("a menu"),
        }
    }

    /// The toolbar, the lane list as shown and the lane's editor: a track
    /// row's menu adds lanes, a lane is chosen with Enter, its mode and
    /// visibility are controls of their own.
    #[test]
    fn the_lanes_and_tools_are_reached_by_the_keyboard() {
        let (mut s, drums) = session();
        let mut view = AutomationView::new(Theme::default());
        assert_eq!(view.accessible_name().as_deref(), Some("Automation"));
        let nodes = view.accessible(SIZE, &s);
        let roles: Vec<AccessRole> = nodes.iter().map(|n| n.role).collect();
        assert_eq!(
            roles,
            [AccessRole::Toolbar, AccessRole::List, AccessRole::Group]
        );
        // The list's rows are the rows shown.
        assert_eq!(nodes[1].children.len(), view.rows(&s).len());
        // "Selected tracks only": the view's own filter, a toggle.
        let only = named(&view, &s, "Selected tracks only");
        assert_eq!(only.role, AccessRole::ToggleButton);
        assert_eq!(only.checked, Some(false));
        s.selection.tracks.clear();
        s.selection.tracks.insert(drums);
        key(&mut view, &mut s, "Selected tracks only", Key::Enter);
        assert!(view.selected_only);
        assert_eq!(named(&view, &s, "Selected tracks only").checked, Some(true));
        assert_eq!(view.accessible(SIZE, &s)[1].children.len(), 2);
        // The track's row: its menu adds a lane.
        let items = menu(key(&mut view, &mut s, "Drums, 1 lane", Key::Enter));
        let mute = items
            .iter()
            .find(|i| i.label == "Mute")
            .and_then(|i| i.action.clone())
            .unwrap_or_else(|| {
                panic!(
                    "{:?}",
                    items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
                )
            });
        s.dispatch(mute).unwrap();
        assert_eq!(view.accessible(SIZE, &s)[1].children.len(), 3);
        // A lane: Enter edits it; its mode's menu; the arranger's eye.
        let lane = named(&view, &s, "Mute on Drums");
        assert_eq!(lane.selected, Some(false));
        key(&mut view, &mut s, "Mute on Drums", Key::Enter);
        assert_eq!(named(&view, &s, "Mute on Drums").selected, Some(true));
        assert!(
            view.accessible(SIZE, &s)[2]
                .label
                .starts_with("Mute on Drums")
        );
        let modes = menu(key(&mut view, &mut s, "Mode: ", Key::Enter));
        assert_eq!(modes.len(), 5);
        let eye = find_node(&view.accessible(SIZE, &s), lane.id)
            .unwrap()
            .children
            .iter()
            .find(|n| n.label == "Shown in the arranger")
            .cloned()
            .unwrap();
        assert_eq!(eye.role, AccessRole::ToggleButton);
        let was = eye.checked.unwrap();
        let nodes = view.accessible(SIZE, &s);
        let (actions, _) = press_key(
            &mut view,
            &nodes,
            eye.id,
            Key::Enter,
            Modifiers::NONE,
            SIZE,
            &s,
        )
        .unwrap();
        for a in actions {
            s.dispatch(a).unwrap();
        }
        let now = find_node(&view.accessible(SIZE, &s), eye.id)
            .and_then(|n| n.checked)
            .unwrap();
        assert_eq!(now, !was);
        // The lane tools act on it: Delete Lane.
        assert!(!named(&view, &s, "Delete lane").disabled);
        key(&mut view, &mut s, "Delete lane", Key::Enter);
        let lanes = &s.project().track(drums).unwrap().automation.lanes;
        assert_eq!(lanes.len(), 1, "the mute lane went");
    }

    /// Points are sliders: Enter adds one at the playhead, the arrows move
    /// a point's value, Enter puts the playhead on it, its menu shapes,
    /// moves and deletes it.
    #[test]
    fn points_are_added_stepped_and_moved_by_the_keyboard() {
        let (mut s, drums) = session();
        let mut view = AutomationView::new(Theme::default());
        s.dispatch(Action::Transport(
            faderframe_session::TransportAction::Locate(MusicalTime::from_quarters(4.0)),
        ))
        .unwrap();
        key(&mut view, &mut s, "Add point at the playhead", Key::Enter);
        let points = volume_lane(&s, drums).curve.points().to_vec();
        assert_eq!(points.len(), 2, "the value before it and the point");
        let at = s.playhead();
        assert!((at.quarters() - 4.0).abs() < 1e-3);
        assert_eq!(points[1].time, at);
        assert!(
            named(&view, &s, "Add point at the playhead").disabled,
            "a point is there now"
        );
        // The second point: a step up, a hundredth of the lane.
        let point = named(&view, &s, "Point 2 at ");
        assert_eq!(point.role, AccessRole::Slider);
        let before = points[1].value;
        key(&mut view, &mut s, "Point 2 at ", Key::Up);
        let after = volume_lane(&s, drums).curve.points()[1].value;
        assert!(after > before, "{before} → {after}");
        // Enter: the playhead on it.
        key(&mut view, &mut s, "Point 1 at ", Key::Enter);
        assert_eq!(s.playhead(), MusicalTime::ZERO);
        // Its menu: later by a sixteenth.
        let items = menu(key(&mut view, &mut s, "Point 2 at ", Key::Menu));
        let later = items
            .iter()
            .find(|i| i.label == "Move Later by a Sixteenth")
            .and_then(|i| i.action.clone())
            .unwrap();
        s.dispatch(later).unwrap();
        assert_eq!(
            volume_lane(&s, drums).curve.points()[1].time,
            MusicalTime::from_quarters(at.quarters() + 0.25)
        );
        assert!(items.iter().any(|i| i.label == "Delete Point"));
        assert!(items.iter().any(|i| i.label == "Step (hold)"));
    }

    /// Without a lane the lane tools are there but unavailable.
    #[test]
    fn the_lane_tools_wait_for_a_lane() {
        let s = Session::new(
            faderframe_project::Project::new("Empty", 48_000),
            None,
            EngineConfig::default(),
        )
        .unwrap();
        let view = AutomationView::new(Theme::default());
        for b in ["Write value", "Thin", "Clear", "Delete lane"] {
            assert!(named(&view, &s, b).disabled, "{b}");
        }
        assert!(!named(&view, &s, "All lanes read").disabled);
        let nodes = view.accessible(SIZE, &s);
        assert!(
            nodes.iter().all(|n| n.role != AccessRole::Group),
            "no editor"
        );
    }
}
