#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Project, Track, TrackColor, TrackKind};
use faderframe_ui_canvas::{Key, Modifiers, RecordingPainter};

const SIZE: Size = Size::new(1200.0, 460.0);

fn session() -> (Session, TrackId) {
    let mut p = Project::new("Game", 48_000);
    let drums: TrackId = p.ids.allocate();
    p.tracks.insert(
        0,
        Track::new(drums, TrackKind::Audio, "Drums", TrackColor::palette(0)),
    );
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    for (a, b) in [(0.0, 16.0), (16.0, 32.0)] {
        s.dispatch(Action::AddSection {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        })
        .unwrap();
    }
    (s, drums)
}

fn press(
    view: &mut AdaptiveView,
    s: &Session,
    pos: Point,
    button: PointerButton,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerDown {
            pos,
            button,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        SIZE,
        s,
        &mut cx,
    );
    (actions, requests)
}

fn button(s: &Session, hit: Hit) -> Point {
    AdaptiveView::buttons(SIZE, s)
        .into_iter()
        .find(|(h, _, _)| *h == hit)
        .unwrap()
        .1
        .center()
}

fn labels(items: &[MenuItem<Action>]) -> Vec<String> {
    items.iter().map(|m| m.label.clone()).collect()
}

#[test]
fn an_empty_score_says_how_to_begin_and_makes_states_of_sections() {
    let (s, _) = session();
    let mut view = AdaptiveView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.balanced_clips());
    let texts = p.texts();
    for t in [
        "Interactive Music",
        "▶ Preview",
        "+ State ▾",
        "Export…",
        "States",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    assert!(texts.iter().any(|t| t.starts_with("No states yet")));
    let (_, req) = press(
        &mut view,
        &s,
        button(&s, Hit::AddState),
        PointerButton::Primary,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("a menu")
    };
    assert_eq!(
        items[0].action,
        Some(Action::Adaptive(AdaptiveOp::StatesFromSections))
    );
    assert!(labels(&items).contains(&"Section".to_string()));
    // Export asks for a folder.
    let (_, req) = press(
        &mut view,
        &s,
        button(&s, Hit::Export),
        PointerButton::Primary,
    );
    assert!(matches!(
        req.first(),
        Some(HostRequest::ChooseFiles {
            choice: FileChoice::Folder { .. },
            ..
        })
    ));
}

#[test]
fn states_are_buttons_rules_open_what_they_do_and_parameters_slide() {
    let (mut s, drums) = session();
    s.dispatch(Action::Adaptive(AdaptiveOp::StatesFromSections))
        .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::AddLayer {
        name: Some("Drums".into()),
        tracks: vec![drums],
    }))
    .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::AddParameter { name: None }))
        .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::AddRule {
        from: RuleEnd::Any,
        to: RuleEnd::Any,
    }))
    .unwrap();
    let mut view = AdaptiveView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    for t in ["Any → Any", "Drums", "Intensity 0.00", "Layers", "Stingers"] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    // A state's button: go there.
    let first = s.interactive().states[0].id;
    let (actions, _) = press(
        &mut view,
        &s,
        AdaptiveView::state_rect(0, SIZE).center(),
        PointerButton::Primary,
    );
    assert_eq!(actions, [Action::Adaptive(AdaptiveOp::Go(first))]);
    // A rule's click: its settings.
    let (_, req) = press(
        &mut view,
        &s,
        AdaptiveView::rule_rect(0, SIZE).center(),
        PointerButton::Primary,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("a menu")
    };
    let l = labels(&items);
    for want in [
        "From",
        "To",
        "Leave On",
        "Fade Out",
        "Fade In",
        "Transition Segment",
        "Destination Starts",
        "Remove Rule",
    ] {
        assert!(l.contains(&want.to_string()), "{want} in {l:?}");
    }
    let leave = items.iter().find(|m| m.label == "Leave On").unwrap();
    let exit = leave
        .children
        .iter()
        .find(|m| m.label == SyncPoint::Exit.label())
        .unwrap();
    let Some(Action::Adaptive(AdaptiveOp::SetRule(r))) = &exit.action else {
        panic!("sets the rule")
    };
    assert_eq!(r.sync, SyncPoint::Exit);
    // The parameter's slider: its value, and a drag on.
    let (_, row) = AdaptiveView::right_rows(SIZE, &s)
        .into_iter()
        .find(|(h, _)| *h == Hit::Parameter(0))
        .unwrap();
    let slider = AdaptiveView::slider(row);
    let id = s.interactive().parameters[0].id;
    let (actions, _) = press(
        &mut view,
        &s,
        Point::new(slider.x + slider.w * 0.75, slider.y + slider.h / 2.0),
        PointerButton::Primary,
    );
    let Some(Action::Adaptive(AdaptiveOp::SetValue { parameter, value })) = actions.first() else {
        panic!("{actions:?}")
    };
    assert_eq!(*parameter, id);
    assert!((value - 0.75).abs() < 0.02);
    // F2 over a state renames it.
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerMove {
            pos: AdaptiveView::state_rect(1, SIZE).center(),
            modifiers: Modifiers::NONE,
            dragging: false,
        },
        SIZE,
        &s,
        &mut cx,
    );
    view.event(
        &ViewEvent::Key {
            key: Key::F(2),
            modifiers: Modifiers::NONE,
        },
        SIZE,
        &s,
        &mut cx,
    );
    let Some(HostRequest::TextInput {
        commit, initial, ..
    }) = requests.pop()
    else {
        panic!("a prompt")
    };
    assert_eq!(initial, s.interactive().states[1].name);
    let Some(Action::Adaptive(AdaptiveOp::SetState(st))) = commit("Combat") else {
        panic!()
    };
    assert_eq!(st.name, "Combat");
}

#[test]
fn a_state_menu_sets_its_loop_once_and_start() {
    let (mut s, _) = session();
    s.dispatch(Action::Adaptive(AdaptiveOp::StatesFromSections))
        .unwrap();
    let mut view = AdaptiveView::new(Theme::default());
    let (_, req) = press(
        &mut view,
        &s,
        AdaptiveView::state_rect(0, SIZE).center(),
        PointerButton::Secondary,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("a menu")
    };
    let entry = items
        .iter()
        .find(|m| m.label == "Loop Begins (Entry Cue)")
        .unwrap();
    let one_bar = entry
        .children
        .iter()
        .find(|m| m.label == "After 1 Bar")
        .unwrap();
    let Some(Action::Adaptive(AdaptiveOp::SetState(st))) = &one_bar.action else {
        panic!()
    };
    assert_eq!(st.entry, MusicalTime::from_quarters(4.0));
    let once = items.iter().find(|m| m.label == "Plays Once").unwrap();
    let Some(Action::Adaptive(AdaptiveOp::SetState(st))) = &once.action else {
        panic!()
    };
    assert!(!st.looping);
    assert!(
        items
            .iter()
            .any(|m| m.label == "The Music Starts Here" && m.checked == Some(true))
    );
}
