#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_core::ParameterId;
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::synth::id as synth;
use faderframe_project::modulation::ModTarget;
use faderframe_session::SelectMode;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SIZE: Size = Size::new(900.0, 300.0);

fn run(
    view: &mut ModulatorsView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn down(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

fn wheel(pos: Point, dy: f32) -> ViewEvent {
    ViewEvent::Scroll {
        pos,
        dx: 0.0,
        dy,
        modifiers: Modifiers::NONE,
        precise: false,
    }
}

/// The demo with its Lead Synth selected and the view.
fn setup() -> (Session, TrackId, ModulatorsView) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .id;
    s.dispatch(Action::SelectTracks {
        tracks: vec![lead],
        mode: SelectMode::Replace,
    })
    .unwrap();
    (s, lead, ModulatorsView::new(Theme::default()))
}

fn dispatch_all(s: &mut Session, actions: Vec<Action>) {
    for a in actions {
        s.dispatch(a).unwrap();
    }
}

/// Add a modulator of `kind` through the Add menu.
fn add(s: &mut Session, view: &mut ModulatorsView, kind: &str) {
    let (_, req) = run(view, down(ModulatorsView::add_rect(SIZE).center(), 1), s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("no menu");
    };
    let item = items.into_iter().find(|i| i.label == kind).unwrap();
    s.dispatch(item.action.unwrap()).unwrap();
}

#[test]
fn knobs_turn_rates_through_the_divisions_and_free_hz() {
    let mut lfo = ModSource::defaults()[0].clone();
    for i in 0..ModRate::DIVISIONS.len() {
        let n = i as f32 / (ModRate::DIVISIONS.len() - 1) as f32;
        ctl_set(&mut lfo, Ctl::Rate, n);
        assert_eq!(
            rate_of(&lfo),
            Some(ModRate::Sync {
                beats: ModRate::DIVISIONS[i].1
            })
        );
        assert!((ctl_get(&lfo, Ctl::Rate) - n).abs() < 1e-6);
    }
    set_rate(&mut lfo, ModRate::Hz { hz: 2.0 });
    let n = ctl_get(&lfo, Ctl::Rate);
    ctl_set(&mut lfo, Ctl::Rate, n);
    assert_eq!(ctl_text(&lfo, Ctl::Rate), "2.00 Hz");
    let mut f = ModSource::defaults()[1].clone();
    ctl_set(&mut f, Ctl::Gain, 0.75);
    assert_eq!(ctl_text(&f, Ctl::Gain), "+12 dB");
}

#[test]
fn a_card_adds_maps_routes_and_drags() {
    let (mut s, lead, mut view) = setup();
    view.paint(&mut RecordingPainter::new(), SIZE, &s, &Theme::default());
    add(&mut s, &mut view, "LFO");
    let m = s.project().track(lead).unwrap().modulators[0].clone();
    let card = view.card(0, &m, SIZE);
    // Map: the session learns.
    let (a, _) = run(&mut view, down(card.map.center(), 1), &s);
    assert_eq!(a, [Action::LearnModulation(Some((lead, m.id)))]);
    dispatch_all(&mut s, a);
    let (a, _) = run(&mut view, down(card.map.center(), 1), &s);
    assert_eq!(a, [Action::LearnModulation(None)]);
    dispatch_all(&mut s, a);
    // + Target lists the fader, the pan and the synth's cutoff.
    let (_, req) = run(&mut view, down(card.add_route.center(), 1), &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("no menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels[..2], ["Volume", "Pan"]);
    let cutoff = items
        .into_iter()
        .find(|i| i.label == "FaderFrame Synth · Cutoff")
        .unwrap();
    s.dispatch(cutoff.action.unwrap()).unwrap();
    let m = s.project().track(lead).unwrap().modulators[0].clone();
    let plugin = s.project().track(lead).unwrap().inserts[0].id;
    assert_eq!(
        m.routes[0].target,
        ModTarget::Plugin {
            plugin,
            parameter: ParameterId(synth::CUTOFF)
        }
    );
    // A depth drag: one gesture.
    let card = view.card(0, &m, SIZE);
    let (_, _, depth, _) = card.routes[0];
    let mut actions = run(&mut view, down(depth.center(), 1), &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(depth.center().x + depth.w / 4.0, depth.center().y),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            &s,
        )
        .0,
    );
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerUp {
                pos: depth.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            &s,
        )
        .0,
    );
    assert!(matches!(actions.first(), Some(Action::BeginGesture(_))));
    assert_eq!(actions.last(), Some(&Action::EndGesture));
    dispatch_all(&mut s, actions);
    let depth_now = s.project().track(lead).unwrap().modulators[0].routes[0].depth;
    assert!((depth_now - 0.75).abs() < 0.02, "{depth_now}");
    // The wheel over the rate knob turns it; elsewhere on the card it does
    // not scroll.
    let m = s.project().track(lead).unwrap().modulators[0].clone();
    let card = view.card(0, &m, SIZE);
    let rate = card.knobs.iter().find(|k| k.0 == Ctl::Rate).unwrap().1;
    let (a, _) = run(&mut view, wheel(rate.center(), -1.0), &s);
    assert!(matches!(a.as_slice(), [Action::SetModulator { .. }]));
    let (a, _) = run(&mut view, wheel(card.display.center(), 3.0), &s);
    assert!(a.is_empty());
    assert_eq!(view.scroll, 0.0);
}

#[test]
fn steps_are_drawn_and_a_macro_dragged() {
    let (mut s, lead, mut view) = setup();
    add(&mut s, &mut view, "Steps");
    add(&mut s, &mut view, "Macro");
    let mods = s.project().track(lead).unwrap().modulators.clone();
    // Draw across the top of the first half of the steps: all to +1.
    let d = view.card(0, &mods[0], SIZE).display;
    let y = d.y + 4.0;
    let mut actions = run(&mut view, down(Point::new(d.x + 6.0, y), 1), &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(d.x + 4.0 + (d.w - 18.0) * 0.45, y),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            &s,
        )
        .0,
    );
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerUp {
                pos: d.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            &s,
        )
        .0,
    );
    dispatch_all(&mut s, actions);
    let ModSource::Steps { steps, .. } = &s.project().track(lead).unwrap().modulators[0].source
    else {
        panic!()
    };
    assert_eq!(steps[..4], [1.0, 1.0, 1.0, 1.0], "{steps:?}");
    assert_ne!(steps[7], 1.0);
    // The macro: dragged to three quarters.
    let d = view.card(1, &mods[1], SIZE).display;
    let at = Point::new(d.x + 4.0 + (d.w - 18.0) * 0.75, d.center().y);
    let mut actions = run(&mut view, down(at, 1), &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerUp {
                pos: at,
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            &s,
        )
        .0,
    );
    dispatch_all(&mut s, actions);
    assert_eq!(
        s.project().track(lead).unwrap().modulators[1].source,
        ModSource::Macro { value: 0.75 }
    );
    // The panel paints them with their names.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(texts.contains(&"Steps") && texts.contains(&"Macro"));
}
