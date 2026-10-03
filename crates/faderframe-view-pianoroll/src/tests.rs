use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::{Action, KeyFold, PianoRollSettings};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, Key, Modifiers, PointerButton, RecordingPainter, ViewEvent,
};

const SIZE: Size = Size {
    w: 1200.0,
    h: 700.0,
};

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

/// Run an event and apply the actions; returns the actions and requests.
fn run(
    view: &mut PianoRollView,
    ev: ViewEvent,
    s: &mut Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    {
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, SIZE, s, &mut cx);
    }
    for a in &actions {
        s.dispatch(a.clone()).unwrap();
    }
    (actions, requests)
}

fn paint(view: &mut PianoRollView, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p
}

fn down(pos: Point, mods: Modifiers, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: mods,
        clicks,
    }
}

fn drag(pos: Point, mods: Modifiers) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: mods,
        dragging: true,
    }
}

fn up(pos: Point, mods: Modifiers) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: mods,
    }
}

fn notes(s: &Session) -> Vec<MidiNote> {
    let id = s.editor_clip().unwrap();
    s.project()
        .clip(id)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone()
}

/// Scroll so `key` is visible; returns the y in the middle of its row.
fn row_y(view: &mut PianoRollView, s: &Session, key: u8) -> f32 {
    paint(view, s);
    view.set_scroll(
        faderframe_ui_canvas::ScrollAxis::Vertical,
        view.row_of(key).unwrap() as f32 * view.row_h - 120.0,
    );
    paint(view, s);
    view.y_of(key).unwrap() + view.row_h / 2.0
}

#[test]
fn note_names_and_keys() {
    assert_eq!(note_name(60), "C4");
    assert_eq!(note_name(69), "A4");
    assert_eq!(note_name(0), "C-1");
    assert!(is_black(61) && !is_black(64));
}

#[test]
fn paints_the_editor_and_the_empty_state() {
    let s = session();
    let mut view = PianoRollView::new(Theme::default());
    let p = paint(&mut view, &s);
    let texts = p.texts();
    assert!(texts.iter().any(|t| t.starts_with('C')), "key names");
    assert!(
        texts.contains(&"Select") && texts.contains(&"Draw"),
        "toolbar"
    );
    assert!(texts.iter().any(|t| t.starts_with("VELOCITY")), "lane");
    assert!(p.balanced_clips());
    let mut empty = Session::new(
        faderframe_project::Project::new("x", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    empty.tick(0.0);
    let p = paint(&mut view, &empty);
    assert!(p.texts()[0].starts_with("Double-click"));
}

#[test]
fn pencil_draws_a_note_whose_length_follows_the_drag() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    view.set_tool(Tool::Pencil);
    let before = notes(&s).len();
    let y = row_y(&mut view, &s, 40);
    let x = view.x_of(MusicalTime::from_quarters(0.1));
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    assert_eq!(notes(&s).len(), before, "previewed, not added yet");
    let x2 = view.x_of(MusicalTime::from_quarters(3.0));
    run(&mut view, drag(Point::new(x2, y), Modifiers::NONE), &mut s);
    run(&mut view, up(Point::new(x2, y), Modifiers::NONE), &mut s);
    let ns = notes(&s);
    assert_eq!(ns.len(), before + 1);
    let n = ns.iter().find(|n| n.key == 40).unwrap();
    assert_eq!(n.start, MusicalTime::ZERO, "snapped to the grid");
    assert_eq!(n.length, MusicalTime::from_quarters_i(3));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(notes(&s).len(), before, "one undo step");
}

#[test]
fn double_click_adds_and_chords_follow_the_scale() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let pr = PianoRollSettings {
        scale: faderframe_project::midi_ops::Scale::new(
            0,
            faderframe_project::midi_ops::ScaleKind::Major,
        ),
        chord: faderframe_project::midi_ops::ChordKind::ScaleTriad,
        ..Default::default()
    };
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    let y = row_y(&mut view, &s, 38);
    let x = view.x_of(MusicalTime::from_quarters(2.0));
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 2),
        &mut s,
    );
    let mut keys: Vec<u8> = notes(&s)
        .iter()
        .filter(|n| n.start == MusicalTime::from_quarters(2.0))
        .map(|n| n.key)
        .collect();
    keys.sort();
    assert!(
        keys.starts_with(&[38, 41, 45]),
        "D minor triad in C: {keys:?}"
    );
}

#[test]
fn dragging_moves_and_alt_dragging_copies() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let n = notes(&s)[0];
    let y = row_y(&mut view, &s, n.key);
    // (Grab the body: the first pixels are the start edge.)
    let x = view.x_of(n.start) + 9.0;
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    let target = view.x_of(n.start + MusicalTime::QUARTER) + 9.0;
    let y_up = view.y_of(n.key + 2).unwrap() + view.row_h / 2.0;
    run(
        &mut view,
        drag(Point::new(target, y_up), Modifiers::NONE),
        &mut s,
    );
    assert_eq!(notes(&s)[0], n, "model unchanged during the drag");
    let (a, _) = run(
        &mut view,
        up(Point::new(target, y_up), Modifiers::NONE),
        &mut s,
    );
    assert!(matches!(a.last(), Some(Action::NoteOperation { .. })));
    let moved = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    assert_eq!(moved.start, n.start + MusicalTime::QUARTER);
    assert_eq!(moved.key, n.key + 2);

    // Alt: a copy, the original stays.
    let count = notes(&s).len();
    let x = view.x_of(moved.start) + 9.0;
    let y = view.y_of(moved.key).unwrap() + view.row_h / 2.0;
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    run(&mut view, down(Point::new(x, y), alt, 1), &mut s);
    let x2 = view.x_of(moved.start + MusicalTime::from_quarters_i(2)) + 9.0;
    run(&mut view, drag(Point::new(x2, y), alt), &mut s);
    run(&mut view, up(Point::new(x2, y), alt), &mut s);
    assert_eq!(notes(&s).len(), count + 1);
    assert!(
        notes(&s)
            .iter()
            .any(|m| m.id == moved.id && m.start == moved.start)
    );
}

#[test]
fn rubber_band_selects_and_shortcuts_edit() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let l = view.layout(SIZE);
    // A band over the whole visible grid.
    let from = Point::new(l.grid.x + 2.0, l.grid.y + 2.0);
    let to = Point::new(l.grid.right() - 2.0, l.grid.bottom() - 2.0);
    run(&mut view, down(from, Modifiers::NONE, 1), &mut s);
    run(&mut view, drag(to, Modifiers::NONE), &mut s);
    run(&mut view, up(to, Modifiers::NONE), &mut s);
    let visible = notes(&s)
        .iter()
        .filter(|n| view.note_rect(n).is_some_and(|r| r.intersects(&l.grid)))
        .count();
    assert!(visible > 0);
    assert_eq!(s.selection.notes.len(), visible);
    // Ctrl+A, Up (transpose), Q (quantize), M (mute), Delete.
    let key = |k: Key, m: Modifiers| ViewEvent::Key {
        key: k,
        modifiers: m,
    };
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    run(&mut view, key(Key::Char('a'), ctrl), &mut s);
    assert_eq!(s.selection.notes.len(), notes(&s).len());
    let before: Vec<u8> = notes(&s).iter().map(|n| n.key).collect();
    run(&mut view, key(Key::Up, Modifiers::NONE), &mut s);
    let after: Vec<u8> = notes(&s).iter().map(|n| n.key).collect();
    assert!(
        before
            .iter()
            .zip(&after)
            .all(|(a, b)| *b == (a + 1).min(127))
    );
    run(&mut view, key(Key::Char('m'), Modifiers::NONE), &mut s);
    assert!(notes(&s).iter().all(|n| n.muted));
    run(&mut view, key(Key::Delete, Modifiers::NONE), &mut s);
    assert!(notes(&s).is_empty());
}

#[test]
fn velocity_stems_and_controller_lanes() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let n = notes(&s)[0];
    let l = view.layout(SIZE);
    let area = PianoRollView::lane_value_rect(l.lane);
    let x = view.x_of(n.start);
    let y0 = area.bottom() - area.h * n.velocity as f32 / 127.0;
    run(
        &mut view,
        down(Point::new(x, y0), Modifiers::NONE, 1),
        &mut s,
    );
    run(
        &mut view,
        drag(Point::new(x, area.y - 30.0), Modifiers::NONE),
        &mut s,
    );
    run(
        &mut view,
        up(Point::new(x, area.y - 30.0), Modifiers::NONE),
        &mut s,
    );
    let v = notes(&s)
        .into_iter()
        .find(|m| m.id == n.id)
        .unwrap()
        .velocity;
    assert_eq!(v, 127, "dragged to the top");

    // The mod wheel lane: draw a line.
    let mut pr = s.editor.piano;
    pr.lane = Some((faderframe_project::MidiController::MOD_WHEEL, 0));
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let a = Point::new(view.x_of(MusicalTime::ZERO) + 1.0, area.bottom());
    let b = Point::new(view.x_of(MusicalTime::from_quarters_i(2)), area.y);
    run(&mut view, down(a, shift, 1), &mut s);
    run(&mut view, drag(b, shift), &mut s);
    run(&mut view, up(b, shift), &mut s);
    let id = s.editor_clip().unwrap();
    let m = s.project().clip(id).unwrap().as_midi().unwrap().clone();
    let lane = m
        .lane(faderframe_project::MidiController::MOD_WHEEL, 0)
        .unwrap();
    assert!(lane.points.len() > 10);
    assert!(lane.points.first().unwrap().value < 10);
    assert!(lane.points.last().unwrap().value > 115);
}

#[test]
fn toolbar_switches_tools_and_opens_menus() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let l = view.layout(SIZE);
    let items = view.toolbar_items(l.toolbar, &s);
    let rect_of = |item: crate::toolbar::Item| items.iter().find(|i| i.0 == item).unwrap().1;
    run(
        &mut view,
        down(
            rect_of(crate::toolbar::Item::Tool(Tool::Eraser)).center(),
            Modifiers::NONE,
            1,
        ),
        &mut s,
    );
    assert_eq!(view.tool(), Tool::Eraser);
    let (_, req) = run(
        &mut view,
        down(
            rect_of(crate::toolbar::Item::Grid).center(),
            Modifiers::NONE,
            1,
        ),
        &mut s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("grid menu")
    };
    assert!(items.iter().any(|i| i.label == "1/16T"));
    let (a, _) = run(
        &mut view,
        down(
            rect_of(crate::toolbar::Item::Ghosts).center(),
            Modifiers::NONE,
            1,
        ),
        &mut s,
    );
    assert!(matches!(a.first(), Some(Action::SetPianoRoll(_))));
}

#[test]
fn folding_shows_only_scale_or_used_keys() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let mut pr = s.editor.piano;
    pr.fold = KeyFold::Used;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let used: std::collections::BTreeSet<u8> = notes(&s).iter().map(|n| n.key).collect();
    assert_eq!(view.rows.len(), used.len());
    pr.fold = KeyFold::Scale;
    pr.scale = faderframe_project::midi_ops::Scale::new(
        0,
        faderframe_project::midi_ops::ScaleKind::MajorPentatonic,
    );
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    assert!(view.rows.len() < 128 && view.rows.len() >= used.len());
    assert!(view.rows.windows(2).all(|w| w[0] > w[1]), "top to bottom");
}
