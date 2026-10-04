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
