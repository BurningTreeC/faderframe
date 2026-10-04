#![allow(clippy::unwrap_used)]

use crate::eq::EqView;
use crate::program_eq::{ProgramEqView, light_at};
use faderframe_core::{ParameterId, PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::eq::{BandParams, Field, band_id, band_index};
use faderframe_plugin_host::program_eq::param;
use faderframe_project::PluginRef;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, Key, Modifiers, Point, PointerButton, RecordingPainter, Size,
    Theme, ViewEvent,
};

const SIZE: Size = Size::new(1080.0, 620.0);

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

fn band(s: &Session, plugin: PluginInstanceId, b: usize) -> BandParams {
    BandParams::read(&s.plugin_tap(plugin).unwrap().params, b)
}

#[test]
fn bands_are_added_dragged_shaped_and_removed_on_the_display() {
    let (mut s, plugin) = session(builtin::EQ, "EQ");
    let mut view = EqView::new(plugin, &Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.balanced_clips());
    // Double-click in the middle of the display: a bell near 1 kHz, +x dB.
    let at = Point::new(540.0, 200.0);
    run(&mut view, &mut s, down(at, PointerButton::Primary, 2), SIZE);
    let b = band(&s, plugin, 0);
    assert!(b.enabled && b.used, "{b:?}");
    assert_eq!(b.kind.name(), "Bell");
    assert!((300.0..3_000.0).contains(&b.freq), "{}", b.freq);
    assert!(b.gain > 1.0, "above the 0 dB line: {}", b.gain);
    assert_eq!(s.history().undo_label(), Some("Add EQ Band"));
    // Its node is where the click was: drag it right and down, one step.
    run(&mut view, &mut s, down(at, PointerButton::Primary, 1), SIZE);
    run(
        &mut view,
        &mut s,
        drag(Point::new(at.x + 100.0, at.y + 60.0)),
        SIZE,
    );
    run(
        &mut view,
        &mut s,
        up(
            Point::new(at.x + 100.0, at.y + 60.0),
            PointerButton::Primary,
        ),
        SIZE,
    );
    let moved = band(&s, plugin, 0);
    assert!(moved.freq > b.freq * 1.5, "{} → {}", b.freq, moved.freq);
    assert!(moved.gain < b.gain - 2.0);
    assert_eq!(s.history().undo_label(), Some("EQ Band"));
    s.dispatch(Action::Undo).unwrap();
    assert!(
        (band(&s, plugin, 0).freq - b.freq).abs() < 1.0,
        "one undo step"
    );
    // Scrolling on the node narrows it.
    let q = band(&s, plugin, 0).q;
    run(
        &mut view,
        &mut s,
        ViewEvent::Scroll {
            pos: at,
            dx: 0.0,
            dy: -1.0,
            modifiers: Modifiers::NONE,
            precise: false,
        },
        SIZE,
    );
    assert!(band(&s, plugin, 0).q > q);
    // Right-click: the band's menu, the types first.
    let req = run(
        &mut view,
        &mut s,
        down(at, PointerButton::Secondary, 1),
        SIZE,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu")
    };
    assert!(items.iter().any(|i| i.label == "Low Shelf"));
    let shelf = items.iter().find(|i| i.label == "Low Shelf").unwrap();
    s.dispatch(shelf.action.clone().unwrap()).unwrap();
    assert_eq!(band(&s, plugin, 0).kind.name(), "Low Shelf");
    // Double-click the node: bypassed (still shown), and back.
    let node = Point::new(at.x, at.y);
    run(
        &mut view,
        &mut s,
        down(node, PointerButton::Primary, 2),
        SIZE,
    );
    let b = band(&s, plugin, 0);
    assert!(b.used && !b.enabled, "bypassed");
    // Delete removes the selected band.
    run(
        &mut view,
        &mut s,
        ViewEvent::Key {
            key: Key::Delete,
            modifiers: Modifiers::NONE,
        },
        SIZE,
    );
    assert!(!band(&s, plugin, 0).used);
    // Near the bottom of the range a double-click adds a low cut.
    run(
        &mut view,
        &mut s,
        down(Point::new(60.0, 300.0), PointerButton::Primary, 2),
        SIZE,
    );
    assert_eq!(band(&s, plugin, 0).kind.name(), "Low Cut");
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"Band 1"), "the band panel shows it");
}

#[test]
fn the_band_panel_knobs_edit_the_selected_band() {
    let (mut s, plugin) = session(builtin::EQ, "EQ");
    let mut view = EqView::new(plugin, &Theme::default());
    run(
        &mut view,
        &mut s,
        down(Point::new(540.0, 260.0), PointerButton::Primary, 2),
        SIZE,
    );
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    // The gain knob sits after the frequency knob in the panel: drag it up.
    let gain0 = band(&s, plugin, 0).gain;
    let knob = Point::new(380.0 + 70.0 + 32.0, SIZE.h - 96.0 + 48.0);
    run(
        &mut view,
        &mut s,
        down(knob, PointerButton::Primary, 1),
        SIZE,
    );
    run(
        &mut view,
        &mut s,
        drag(Point::new(knob.x, knob.y - 40.0)),
        SIZE,
    );
    run(
        &mut view,
        &mut s,
        up(Point::new(knob.x, knob.y - 40.0), PointerButton::Primary),
        SIZE,
    );
    assert!(band(&s, plugin, 0).gain > gain0 + 5.0);
    // Typed values go through the same parameters.
    assert_eq!(
        s.plugin_tap(plugin)
            .unwrap()
            .params
            .get(band_index(0, Field::Gain)) as f64,
        band(&s, plugin, 0).gain
    );
    let _ = band_id(0, Field::Gain);
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
