#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_project::ClipContent;
use faderframe_ui_canvas::{CanvasView, Modifiers, RecordingPainter};

fn session() -> (Session, ClipId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let clip = s
        .project()
        .clips
        .values()
        .find(|c| c.name == "Melody" && matches!(c.content, ClipContent::Midi(_)))
        .unwrap()
        .id;
    s.dispatch(Action::OpenClipEditor(clip)).unwrap();
    (s, clip)
}

/// Run one event; the actions and host requests it made.
fn run(
    view: &mut EventsView,
    ev: ViewEvent,
    size: Size,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, size, s, &mut cx);
    (actions, requests)
}

fn down(pos: Point, clicks: u32, modifiers: Modifiers) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers,
        clicks,
    }
}

fn apply(s: &mut Session, actions: Vec<Action>) {
    for a in actions {
        s.dispatch(a).unwrap();
    }
}

const SIZE: Size = Size::new(720.0, 480.0);

#[test]
fn the_list_shows_the_clips_events_and_edits_them() {
    let (mut s, clip) = session();
    let mut view = EventsView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(texts.contains(&"Event List"));
    assert!(texts.contains(&"Note"));
    let rows = view.rows(&s);
    let first = rows[0].clone();
    assert_eq!(first.kind, EventKind::Note);
    assert!(texts.contains(&note_name(first.data1.unwrap() as u8).as_str()));

    // Dragging the velocity up 5 steps: committed once, on release.
    let cell = EventsView::cell(Column::Data2, view.row_rect(0, SIZE)).center();
    let (actions, _) = run(&mut view, down(cell, 1, Modifiers::NONE), SIZE, &s);
    assert!(
        matches!(actions.as_slice(), [Action::SelectNotes { .. }]),
        "the row is selected"
    );
    apply(&mut s, actions);
    let moved = Point::new(cell.x, cell.y - 5.0 * DRAG_PX);
    let (actions, _) = run(
        &mut view,
        ViewEvent::PointerMove {
            pos: moved,
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        SIZE,
        &s,
    );
    assert!(actions.is_empty(), "nothing committed while dragging");
    let (actions, _) = run(
        &mut view,
        ViewEvent::PointerUp {
            pos: moved,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        SIZE,
        &s,
    );
    let velocity = i64::from(first.data2.unwrap());
    assert_eq!(
        actions,
        [Action::EditMidiEvents {
            clip,
            events: vec![first.event],
            field: EventField::Data2,
            value: EventValue::Number((velocity + 5).min(127)),
        }]
    );
    apply(&mut s, actions);
    assert_eq!(
        view.rows(&s)[0].data2.map(i64::from),
        Some((velocity + 5).min(127))
    );

    // Typing a key by name.
    let key = EventsView::cell(Column::Data1, view.row_rect(0, SIZE)).center();
    let (_, requests) = run(&mut view, down(key, 2, Modifiers::NONE), SIZE, &s);
    let Some((initial, commit)) = requests.into_iter().find_map(|r| match r {
        HostRequest::TextInput {
            initial, commit, ..
        } => Some((initial, commit)),
        _ => None,
    }) else {
        panic!("a text entry");
    };
    assert_eq!(initial, note_name(first.data1.unwrap() as u8));
    let action = commit("F#3").unwrap();
    assert_eq!(
        action,
        Action::EditMidiEvents {
            clip,
            events: vec![first.event],
            field: EventField::Data1,
            value: EventValue::Number(54),
        }
    );
    assert!(commit("H9").is_none(), "not a key");
    s.dispatch(action).unwrap();

    // "+ Add" → a mod wheel value; then only controllers shown.
    let add = EventsView::header(SIZE).add.center();
    let (_, requests) = run(&mut view, down(add, 1, Modifiers::NONE), SIZE, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
        panic!("the add menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Note", "Control Change", "Pitch Bend", "Channel Pressure"]
    );
    let wheel = items[1]
        .children
        .iter()
        .find(|i| i.label == "CC 1 · Mod Wheel")
        .unwrap();
    s.dispatch(wheel.action.clone().unwrap()).unwrap();
    let notes = EventsView::header(SIZE).notes.center();
    run(&mut view, down(notes, 1, Modifiers::NONE), SIZE, &s);
    let rows = view.rows(&s);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].kind,
        EventKind::Controller(MidiController::MOD_WHEEL)
    );
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"Mod Wheel"));

    // Its row menu: the channel of the selection.
    let row = view.row_rect(0, SIZE).center();
    let (actions, requests) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: row,
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        SIZE,
        &s,
    );
    apply(&mut s, actions);
    let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
        panic!("the row menu");
    };
    let channel = items.iter().find(|i| i.label == "Channel").unwrap();
    assert_eq!(channel.children[0].checked, Some(true));
    s.dispatch(channel.children[4].action.clone().unwrap())
        .unwrap();
    assert_eq!(view.rows(&s)[0].channel, Some(4));

    // Everything shown again; Ctrl+A, Delete: an empty clip.
    run(&mut view, down(notes, 1, Modifiers::NONE), SIZE, &s);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Char('a'),
            modifiers: ctrl,
        },
        SIZE,
        &s,
    );
    apply(&mut s, actions);
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Delete,
            modifiers: Modifiers::NONE,
        },
        SIZE,
        &s,
    );
    assert!(matches!(
        actions.as_slice(),
        [Action::RemoveMidiEvents { .. }]
    ));
    apply(&mut s, actions);
    assert!(view.rows(&s).is_empty());
}

#[test]
fn a_click_in_the_first_column_moves_the_playhead() {
    let (s, _) = session();
    let mut view = EventsView::new(Theme::default());
    let rows = view.rows(&s);
    let at = EventsView::cell(Column::Locate, view.row_rect(2, SIZE)).center();
    let (actions, _) = run(&mut view, down(at, 1, Modifiers::NONE), SIZE, &s);
    assert_eq!(
        actions[0],
        Action::Transport(TransportAction::Locate(rows[2].at))
    );
}

#[test]
fn typed_keys_and_lengths() {
    assert_eq!(parse_key("C4"), Some(60));
    assert_eq!(parse_key("c#4"), Some(61));
    assert_eq!(parse_key("Bb3"), Some(58));
    assert_eq!(parse_key("C-1"), Some(0));
    assert_eq!(parse_key("G9"), Some(127));
    assert_eq!(parse_key("G#9"), None);
    assert_eq!(parse_key("64"), Some(64));
    for key in 0..=127u8 {
        assert_eq!(parse_key(&note_name(key)), Some(key));
    }
    let q = MusicalTime::from_quarters;
    assert_eq!(parse_length("1.000"), Some(q(1.0)));
    assert_eq!(parse_length("0.480"), Some(q(0.5)));
    assert_eq!(parse_length("2"), Some(q(2.0)));
    assert_eq!(parse_length("1/8"), Some(q(0.5)));
    assert_eq!(parse_length("3/16"), Some(q(0.75)));
    assert_eq!(parse_length("0"), None);
    assert_eq!(parse_length("1.960"), None);
    assert_eq!(length_text(q(1.5)), "1.480");
    assert_eq!(parse_length(&length_text(q(2.25))), Some(q(2.25)));
}
