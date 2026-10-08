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

/// Drag a cell `steps` steps up (committed on release).
fn drag(
    view: &mut EventsView,
    s: &mut Session,
    row: usize,
    col: Column,
    steps: f32,
) -> Vec<Action> {
    let cell = EventsView::cell(col, view.row_rect(row, SIZE)).center();
    let (actions, _) = run(view, down(cell, 1, Modifiers::NONE), SIZE, s);
    apply(s, actions);
    let moved = Point::new(cell.x, cell.y - steps * DRAG_PX);
    let (actions, _) = run(
        view,
        ViewEvent::PointerMove {
            pos: moved,
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        SIZE,
        s,
    );
    assert!(actions.is_empty(), "nothing committed while dragging");
    let (actions, _) = run(
        view,
        ViewEvent::PointerUp {
            pos: moved,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        SIZE,
        s,
    );
    actions
}

/// Type `text` into a cell: the action its entry makes.
fn type_into(
    view: &mut EventsView,
    s: &Session,
    row: usize,
    col: Column,
    text: &str,
) -> (String, Option<Action>) {
    let cell = EventsView::cell(col, view.row_rect(row, SIZE)).center();
    let (_, requests) = run(view, down(cell, 2, Modifiers::NONE), SIZE, s);
    let Some((initial, commit)) = requests.into_iter().find_map(|r| match r {
        HostRequest::TextInput {
            initial, commit, ..
        } => Some((initial, commit)),
        _ => None,
    }) else {
        panic!("a text entry in {col:?}");
    };
    (initial, commit(text))
}

/// "+ Add" → the item `path` leads to.
fn add(view: &mut EventsView, s: &mut Session, path: &[&str]) {
    let add = EventsView::header(SIZE).add.center();
    let (_, requests) = run(view, down(add, 1, Modifiers::NONE), SIZE, s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
        panic!("the add menu");
    };
    let mut items = items;
    let mut item = None;
    for (i, label) in path.iter().enumerate() {
        let found = items
            .into_iter()
            .find(|m| m.label.starts_with(label))
            .unwrap_or_else(|| panic!("no {label}"));
        if i + 1 == path.len() {
            item = Some(found);
            break;
        }
        items = found.children;
    }
    s.dispatch(item.unwrap().action.unwrap()).unwrap();
}

fn row_of(view: &EventsView, s: &Session, kind: EventKind) -> (usize, EventRow) {
    view.rows(s)
        .into_iter()
        .enumerate()
        .find(|(_, r)| r.kind == kind)
        .unwrap()
}

const SIZE: Size = Size::new(1000.0, 480.0);

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
    let actions = drag(&mut view, &mut s, 0, Column::Data2, 5.0);
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
    let (initial, action) = type_into(&mut view, &s, 0, Column::Data1, "F#3");
    assert_eq!(initial, note_name(first.data1.unwrap() as u8));
    assert_eq!(
        action,
        Some(Action::EditMidiEvents {
            clip,
            events: vec![first.event],
            field: EventField::Data1,
            value: EventValue::Number(54),
        })
    );
    assert!(type_into(&mut view, &s, 0, Column::Data1, "H9").1.is_none());
    s.dispatch(action.unwrap()).unwrap();

    // The release velocity: none until dragged (from 64), or typed.
    assert_eq!(view.rows(&s)[0].release, None);
    let actions = drag(&mut view, &mut s, 0, Column::Release, 3.0);
    apply(&mut s, actions);
    assert_eq!(view.rows(&s)[0].release, Some(67));
    let (_, cleared) = type_into(&mut view, &s, 0, Column::Release, "");
    s.dispatch(cleared.unwrap()).unwrap();
    assert_eq!(view.rows(&s)[0].release, None);

    // The end typed: the length follows.
    let at = view.rows(&s)[0].at;
    let end = s
        .project()
        .timeline
        .format_bbt(at + MusicalTime::from_quarters(3.0));
    let (_, a) = type_into(&mut view, &s, 0, Column::End, &end);
    s.dispatch(a.unwrap()).unwrap();
    assert_eq!(
        view.rows(&s)[0].length,
        Some(MusicalTime::from_quarters(3.0))
    );

    // M mutes at a click.
    let m = EventsView::cell(Column::Mute, view.row_rect(0, SIZE)).center();
    let (actions, _) = run(&mut view, down(m, 1, Modifiers::NONE), SIZE, &s);
    apply(&mut s, actions);
    assert!(view.rows(&s)[0].muted);

    // "+ Add" → a mod wheel value; then only controllers shown.
    add(&mut view, &mut s, &["Control Change", "CC 1 · Mod Wheel"]);
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
fn every_kind_of_event_can_be_added_and_edited() {
    let (mut s, _) = session();
    let mut view = EventsView::new(Theme::default());
    // A program change: shown 1–128 with its GM name.
    add(&mut view, &mut s, &["Program Change"]);
    let (i, _) = row_of(&view, &s, EventKind::Controller(MidiController::Program));
    let (initial, a) = type_into(&mut view, &s, i, Column::Data2, "41");
    assert_eq!(initial, "1");
    s.dispatch(a.unwrap()).unwrap();
    let (i, r) = row_of(&view, &s, EventKind::Controller(MidiController::Program));
    assert_eq!(r.data2, Some(40));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"GM: Violin"));
    assert!(
        type_into(&mut view, &s, i, Column::Data2, "129")
            .1
            .is_none()
    );

    // Poly pressure: its key moves it to another key.
    add(&mut view, &mut s, &["Poly Pressure"]);
    let kind = view
        .rows(&s)
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                EventKind::Controller(MidiController::PolyPressure { .. })
            )
        })
        .unwrap()
        .kind;
    let (i, _) = row_of(&view, &s, kind);
    let (_, a) = type_into(&mut view, &s, i, Column::Data1, "D4");
    s.dispatch(a.unwrap()).unwrap();
    assert!(view.rows(&s).iter().any(|r| r.kind
        == EventKind::Controller(MidiController::PolyPressure { key: 62 })
        && r.data1 == Some(62)));

    // A note's expression: select the first note, add a pressure point,
    // type and drag its value.
    let first = EventsView::cell(Column::Type, view.row_rect(0, SIZE)).center();
    let (actions, _) = run(&mut view, down(first, 1, Modifiers::NONE), SIZE, &s);
    apply(&mut s, actions);
    add(&mut view, &mut s, &["Note Expression", "Pressure"]);
    let kind = EventKind::Expression(ExpressionKind::Pressure);
    let (i, r) = row_of(&view, &s, kind);
    assert_eq!(r.amount, Some(0.0));
    let (_, a) = type_into(&mut view, &s, i, Column::Data2, "0.5");
    s.dispatch(a.unwrap()).unwrap();
    assert_eq!(row_of(&view, &s, kind).1.amount, Some(0.5));
    let (i, _) = row_of(&view, &s, kind);
    let actions = drag(&mut view, &mut s, i, Column::Data2, 10.0);
    apply(&mut s, actions);
    let v = row_of(&view, &s, kind).1.amount.unwrap();
    assert!((v - 0.6).abs() < 1e-4, "{v}");
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"Note Pressure"));

    // SysEx: its bytes typed in hex.
    add(&mut view, &mut s, &["SysEx"]);
    let (i, r) = row_of(&view, &s, EventKind::Sysex);
    assert_eq!(r.bytes, faderframe_session::midi_events::NEW_SYSEX);
    let (initial, a) = type_into(&mut view, &s, i, Column::Info, "41 10 42 12 40 00 7F 00 41");
    assert_eq!(initial, "F0 7E 7F 06 01 F7");
    s.dispatch(a.unwrap()).unwrap();
    assert_eq!(
        row_of(&view, &s, EventKind::Sysex).1.bytes,
        [
            0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x41, 0xF7
        ]
    );
    let (i, _) = row_of(&view, &s, EventKind::Sysex);
    assert!(
        type_into(&mut view, &s, i, Column::Info, "F0 90 F7")
            .1
            .is_none()
    );
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
fn typed_keys_lengths_and_amounts() {
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
    use ExpressionKind as K;
    assert_eq!(parse_amount(K::Pitch, "+1.50 st"), Some(1.5));
    assert_eq!(parse_amount(K::Volume, "−3.0 dB"), Some(-3.0));
    assert_eq!(parse_amount(K::Pan, "L 30"), Some(-0.3));
    assert_eq!(parse_amount(K::Pan, "C"), Some(0.0));
    assert_eq!(parse_amount(K::Pressure, "2"), Some(1.0), "clamped");
    for k in ExpressionKind::ALL {
        for v in [k.range().0, k.rest(), k.range().1 * 0.5] {
            let back = parse_amount(k, &k.format(v)).unwrap();
            assert!((back - v).abs() < 0.01, "{k:?} {v} → {}", k.format(v));
        }
    }
}
