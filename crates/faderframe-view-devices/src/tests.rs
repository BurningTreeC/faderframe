#![allow(clippy::unwrap_used)]

use crate::program_eq::{ProgramEqView, light_at};
use faderframe_core::{ParameterId, PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::program_eq::param;
use faderframe_project::PluginRef;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, Modifiers, Point, PointerButton, RecordingPainter, Size,
    Theme, ViewEvent,
};

/// The demo with `id` on its first audio track; the plugin's id.
fn session(id: &str, name: &str) -> (Session, PluginInstanceId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let track = s.project().tracks[0].id;
    s.dispatch(Action::InsertPlugin {
        track,
        index: 0,
        plugin: PluginRef::builtin(id, name),
    })
    .unwrap();
    let plugin = s.project().tracks[0].inserts[0].id;
    assert!(s.plugin_tap(plugin).is_some(), "the engine hosts it");
    (s, plugin)
}

fn run(
    view: &mut dyn CanvasView<Session, Action>,
    s: &mut Session,
    ev: ViewEvent,
    size: Size,
) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, size, s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    requests
}

fn down(pos: Point, button: PointerButton, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

fn drag(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn up(pos: Point, button: PointerButton) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn the_program_eq_panel_turns_switches_and_knobs() {
    let (mut s, plugin) = session(builtin::PROGRAM_EQ, "Program EQ");
    let size = Size::new(1160.0, 356.0);
    let mut view = ProgramEqView::new(plugin, &Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.balanced_clips());
    assert!(p.texts().contains(&"PROGRAM EQ"));
    let value = |s: &Session, i: usize| f64::from(s.plugin_tap(plugin).unwrap().params.get(i));
    // At scale 1 the panel starts 34 below the top.
    let panel = |x: f32, y: f32| Point::new(x, y + 34.0);
    // Drag the low boost knob (330, 88) up by its whole range.
    run(
        &mut view,
        &mut s,
        down(panel(330.0, 88.0), PointerButton::Primary, 1),
        size,
    );
    run(&mut view, &mut s, drag(panel(330.0, 88.0 - 130.0)), size);
    run(
        &mut view,
        &mut s,
        up(panel(330.0, -42.0), PointerButton::Primary),
        size,
    );
    assert!((value(&s, param::LOW_BOOST) - 5.0).abs() < 0.2);
    // Click the "30" engraved round the low frequency selector.
    let at = crate::program_eq::selector_angle(1, 4).to_radians();
    let (x, y) = (400.0 + 54.0 * at.sin(), 234.0 - 54.0 * at.cos());
    run(
        &mut view,
        &mut s,
        down(panel(x, y), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, param::LOW_FREQ), 1.0);
    // A click on the power switch throws it off.
    run(
        &mut view,
        &mut s,
        down(panel(985.0, 234.0), PointerButton::Primary, 1),
        size,
    );
    run(
        &mut view,
        &mut s,
        up(panel(985.0, 234.0), PointerButton::Primary),
        size,
    );
    assert_eq!(value(&s, param::POWER), 0.0);
    // The oversampling strip.
    let r = 1160.0 - 24.0 - 4.0 * 38.0 + 17.0;
    run(
        &mut view,
        &mut s,
        down(Point::new(r, 17.0), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, param::OVERSAMPLING), 0.0);
    // Double-click resets a knob.
    run(
        &mut view,
        &mut s,
        down(panel(330.0, 88.0), PointerButton::Primary, 2),
        size,
    );
    assert_eq!(value(&s, param::LOW_BOOST), 0.0);
    assert!(light_at(0.0, 0.0) > light_at(1160.0, 322.0));
    let _ = ParameterId(0);
}

mod guitar_station {
    use super::*;
    use crate::guitar::GuitarView;
    use faderframe_guitar::pedal::Stomp;
    use faderframe_guitar::voice::Pedal;
    use faderframe_plugin_host::devices::guitar::id;
    use faderframe_ui_canvas::MenuItem;

    const SIZE: Size = Size::new(crate::guitar::PANEL_W, crate::guitar::TOTAL_H);

    /// A panel point at scale 1 (the panel starts below the header).
    fn at(x: f32, y: f32) -> Point {
        Point::new(x, y + crate::guitar::HEADER_H)
    }

    fn value(s: &Session, plugin: PluginInstanceId, id: u32) -> f64 {
        f64::from(s.plugin_tap(plugin).unwrap().params.get(id as usize))
    }

    fn click(view: &mut GuitarView, s: &mut Session, p: Point) -> Vec<HostRequest<Action>> {
        let r = run(view, s, down(p, PointerButton::Primary, 1), SIZE);
        run(view, s, up(p, PointerButton::Primary), SIZE);
        r
    }

    /// The action of a menu's item called `label`.
    fn pick(requests: Vec<HostRequest<Action>>, label: &str) -> Action {
        fn find(items: Vec<MenuItem<Action>>, label: &str) -> Option<Action> {
            items.into_iter().find_map(|i| {
                if i.label == label {
                    i.action
                } else {
                    find(i.children, label)
                }
            })
        }
        let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
            panic!("no menu");
        };
        find(items, label).unwrap_or_else(|| panic!("no {label}"))
    }

    #[test]
    fn pedals_are_added_switched_moved_and_removed_on_the_line() {
        let (mut s, plugin) = session(builtin::GUITAR_STATION, "Guitar Station");
        let mut view = GuitarView::new(plugin, &Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.balanced_clips());
        assert!(p.texts().contains(&"BRIT 800"));
        assert!(p.texts().contains(&"ADD PEDAL"));
        // Add two pedals through the empty place's menu.
        let add = |view: &mut GuitarView, s: &mut Session, n: usize, name: &str| {
            let r = crate::guitar::layout::pedal_rect(n);
            let menu = click(view, s, at(r.center().x, r.center().y));
            s.dispatch(pick(menu, name)).unwrap();
        };
        add(&mut view, &mut s, 0, "Green 808");
        add(&mut view, &mut s, 1, "Ram Fuzz");
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::Green808).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::STOMP)),
            Stomp::Pedal(Pedal::BigMuff).index() as f64
        );
        // The footswitch of the first.
        let r = crate::guitar::layout::pedal_rect(0);
        click(&mut view, &mut s, at(r.center().x, r.y + 172.0));
        assert_eq!(value(&s, plugin, id::slot(0, id::ON)), 0.0);
        // Its drive knob, dragged up.
        let knob = at(r.x + 8.0 + (r.w - 16.0) / 6.0, r.y + 14.0 + 46.0 - 4.0);
        let before = value(&s, plugin, id::slot(0, id::P_DRIVE));
        run(
            &mut view,
            &mut s,
            down(knob, PointerButton::Primary, 1),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            drag(Point::new(knob.x, knob.y - 60.0)),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            up(Point::new(knob.x, knob.y - 60.0), PointerButton::Primary),
            SIZE,
        );
        assert!(value(&s, plugin, id::slot(0, id::P_DRIVE)) > before + 0.2);
        // Carry the second pedal before the first: its knobs go with it.
        let second = crate::guitar::layout::pedal_rect(1);
        let grab = at(second.x + 20.0, second.y + 150.0);
        run(
            &mut view,
            &mut s,
            down(grab, PointerButton::Primary, 1),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            drag(Point::new(grab.x - 136.0, grab.y)),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            up(Point::new(grab.x - 136.0, grab.y), PointerButton::Primary),
            SIZE,
        );
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::BigMuff).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::STOMP)),
            Stomp::Pedal(Pedal::Green808).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::ON)),
            0.0,
            "its footswitch moved with it"
        );
        // One undo step brings the order back.
        s.dispatch(Action::Undo).unwrap();
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::Green808).index() as f64
        );
        // Remove the first: the line closes up.
        let menu = run(
            &mut view,
            &mut s,
            down(at(r.center().x, r.y + 137.0), PointerButton::Secondary, 1),
            SIZE,
        );
        s.dispatch(pick(menu, "Remove Pedal")).unwrap();
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::BigMuff).index() as f64
        );
        assert_eq!(value(&s, plugin, id::slot(1, id::STOMP)), 0.0);
        // The amplifier's menu.
        let name = crate::guitar::layout::amp_look_for_tests();
        let menu = click(&mut view, &mut s, at(name.center().x, name.center().y));
        s.dispatch(pick(menu, "American Twin")).unwrap();
        assert_eq!(value(&s, plugin, id::AMP), 8.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.texts().contains(&"AMERICAN TWIN"));
        assert!(p.texts().contains(&"REVERB"), "the Twin's own controls");
        assert!(!p.texts().contains(&"MASTER"), "an AB763 has no master");
    }
}
