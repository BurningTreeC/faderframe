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
    assert!(
        items.iter().any(|i| i.label == "1/256"),
        "grids down to 1/256"
    );
    assert!(items.iter().any(|i| i.label == "Triplet"));
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

/// A track in a tuning of its own: rows are its degrees and cents, and
/// "scale keys only" keeps the keys its keyboard mapping plays.
#[test]
fn a_tuned_track_labels_rows_with_degrees_and_folds_to_its_keys() {
    use faderframe_project::tuning::{KeyboardMap, Tuning};
    let mut s = session();
    let clip = s.editor_clip().unwrap();
    let track = s.project().clip(clip).unwrap().track;
    let mut view = PianoRollView::new(Theme::default());
    let p = paint(&mut view, &s);
    assert!(p.texts().contains(&"C4"), "note names in 12-TET");
    s.dispatch(Action::Edit(faderframe_project::Command::SetTuning {
        track: Some(track),
        tuning: Some(Tuning::equal(19)),
    }))
    .unwrap();
    let p = paint(&mut view, &s);
    let texts = p.texts();
    assert!(texts.contains(&"0 · 0¢"), "the tonic labelled: {texts:?}");
    assert!(!texts.contains(&"C4"));
    assert_eq!(view.key_label(61), "1 · 63¢");
    // Only the white keys play (a mapping leaving the black keys out).
    let mut map = KeyboardMap::standard(7);
    map.size = 12;
    map.octave_degree = 7;
    map.map = vec![
        Some(0),
        None,
        Some(1),
        None,
        Some(2),
        Some(3),
        None,
        Some(4),
        None,
        Some(5),
        None,
        Some(6),
    ];
    let white = Tuning::equal(7).with_mapping(Some(map)).unwrap();
    s.dispatch(Action::Edit(faderframe_project::Command::SetTuning {
        track: Some(track),
        tuning: Some(white),
    }))
    .unwrap();
    let mut pr = s.editor.piano;
    pr.fold = KeyFold::Scale;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let notes: std::collections::BTreeSet<u8> = notes(&s).iter().map(|n| n.key).collect();
    assert!(
        view.rows.iter().all(|k| !is_black(*k) || notes.contains(k)),
        "black keys folded away (unless a note is there)"
    );
    assert!(view.rows.contains(&60) && view.rows.contains(&62));
    assert_eq!(view.key_label(61), "C#4", "an unmapped key keeps its name");
}

#[test]
fn expression_lane_draws_the_notes_under_the_pointer_in_one_step() {
    use faderframe_project::ExpressionKind;
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let mut pr = s.editor.piano;
    pr.expression = Some(ExpressionKind::Pitch);
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let texts: Vec<String> = paint(&mut view, &s)
        .texts()
        .into_iter()
        .map(|t| t.to_string())
        .collect();
    assert!(texts.iter().any(|t| t.starts_with("PITCH ▾")), "{texts:?}");
    let n = notes(&s)[0];
    let l = view.layout(SIZE);
    let area = PianoRollView::lane_value_rect(l.lane);
    // A line over the first note from 0 to the top (+12 semitones).
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let a = Point::new(view.x_of(n.start) + 1.0, area.center().y);
    let b = Point::new(view.x_of(n.end()) - 1.0, area.y);
    run(&mut view, down(a, shift, 1), &mut s);
    run(&mut view, drag(b, shift), &mut s);
    let (actions, _) = run(&mut view, up(b, shift), &mut s);
    assert!(matches!(actions.first(), Some(Action::BeginGesture(_))));
    assert!(matches!(actions.last(), Some(Action::EndGesture)));
    let id = s.editor_clip().unwrap();
    let m = s.project().clip(id).unwrap().as_midi().unwrap().clone();
    let e = m.expression(n.id).expect("expression on the note");
    assert!(e.value_at(ExpressionKind::Pitch, MusicalTime::ZERO).abs() < 0.5);
    assert!(e.value_at(ExpressionKind::Pitch, n.length) > 11.0);
    // Only notes sounding where the drag started got expression.
    assert!(
        m.expressions.iter().all(|x| {
            let o = m.note(x.note).unwrap();
            o.start <= n.start && o.end() > n.start
        }),
        "{:?}",
        m.expressions
    );
    // One undo step for the whole gesture.
    assert_eq!(s.history().undo_label(), Some("Edit Expression"));
    s.dispatch(Action::Undo).unwrap();
    let m = s.project().clip(id).unwrap().as_midi().unwrap().clone();
    assert!(m.expressions.is_empty());
}

#[test]
fn sysex_markers_explain_themselves_and_offer_deleting() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let clip = s.editor_clip().unwrap();
    s.dispatch(Action::AddSysex {
        clip,
        at: MusicalTime::from_quarters_i(1),
        messages: vec![vec![0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7]],
    })
    .unwrap();
    paint(&mut view, &s);
    let l = view.layout(SIZE);
    let at = Point::new(view.x_of(MusicalTime::from_quarters_i(1)), l.ruler.y + 5.0);
    let tip = view.tooltip_at(at, SIZE, &s).unwrap();
    assert!(tip.contains("F0 7E 7F 09 01 F7"), "{tip}");
    let (_, requests) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: at,
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &mut s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests.first() else {
        panic!("a menu: {} requests", requests.len())
    };
    let delete = items
        .iter()
        .find(|i| i.label.starts_with("Delete SysEx"))
        .unwrap();
    s.dispatch(delete.action.clone().unwrap()).unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    assert!(m.sysex.is_empty());
}

#[test]
fn toolbar_wraps_in_narrow_views() {
    let s = session();
    let mut view = PianoRollView::new(Theme::default());
    let row = Theme::default().piano.toolbar_height;
    let mut grid_tops = Vec::new();
    for w in [1600.0, 900.0, 520.0, 380.0] {
        let size = Size::new(w, 700.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        let l = view.layout(size);
        let items = view.toolbar_items(l.toolbar, &s);
        assert_eq!(items.len(), 24, "every control stays reachable at {w}");
        assert!(
            items
                .iter()
                .all(|(_, r, _, _)| r.right() <= w && r.bottom() <= l.toolbar.bottom() + 0.01),
            "inside the toolbar at {w}"
        );
        assert!(l.grid.y >= l.toolbar.bottom());
        grid_tops.push((l.toolbar.h / row).round() as usize);
    }
    assert_eq!(grid_tops[0], 1, "one row when wide: {grid_tops:?}");
    assert!(
        grid_tops[3] >= 4,
        "four or more rows when narrow: {grid_tops:?}"
    );
}

#[test]
fn volume_and_pan_lanes_draw_in_their_units() {
    use faderframe_project::ExpressionKind;
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let mut pr = s.editor.piano;
    pr.expression = Some(ExpressionKind::Volume);
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let n = notes(&s)[0];
    let l = view.layout(SIZE);
    let area = PianoRollView::lane_value_rect(l.lane);
    // From the bottom (−24 dB) to the top (+12 dB) over the first note.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let a = Point::new(view.x_of(n.start) + 1.0, area.bottom());
    let b = Point::new(view.x_of(n.end()) - 1.0, area.y);
    run(&mut view, down(a, shift, 1), &mut s);
    run(&mut view, drag(b, shift), &mut s);
    run(&mut view, up(b, shift), &mut s);
    let id = s.editor_clip().unwrap();
    let m = s.project().clip(id).unwrap().as_midi().unwrap().clone();
    let e = m.expression(n.id).expect("expression on the note");
    assert!((e.value_at(ExpressionKind::Volume, MusicalTime::ZERO) + 24.0).abs() < 1.0);
    assert!(e.value_at(ExpressionKind::Volume, n.length) > 11.0);
    assert!(e.pitch.is_empty(), "only the volume curve");
    // The tooltip reads the lane in dB; the centre of the pan lane is C.
    let tip = view
        .tooltip(Point::new(a.x, area.y + 1.0), SIZE, &s)
        .unwrap();
    assert!(tip.starts_with("Volume +1"), "{tip}");
    let mut pr = s.editor.piano;
    pr.expression = Some(ExpressionKind::Pan);
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let tip = view
        .tooltip(Point::new(a.x, area.center().y), SIZE, &s)
        .unwrap();
    assert!(tip.starts_with("Pan C"), "{tip}");
}

#[test]
fn the_key_and_chord_tracks_show_and_lead_the_scale() {
    use faderframe_project::harmony::{Chord, Key, Quality, Scale as KeyScale};
    use faderframe_project::{ChordEvent, Command, KeyChange};
    let mut s = session();
    // Without the demo's key and chords first.
    s.dispatch(Action::Edit(Command::SetKeys { keys: Vec::new() }))
        .unwrap();
    s.dispatch(Action::Edit(Command::SetChords { chords: Vec::new() }))
        .unwrap();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let plain = view.grid_top();
    // The melody clip starts at bar 5: C major there, A minor from bar 7;
    // Am then F under it.
    let q = MusicalTime::from_quarters;
    s.dispatch(Action::Edit(Command::SetKeys {
        keys: vec![
            KeyChange {
                at: q(0.0),
                key: Key::new(0, KeyScale::Major),
            },
            KeyChange {
                at: q(24.0),
                key: Key::new(9, KeyScale::Minor),
            },
        ],
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetChords {
        chords: vec![
            ChordEvent {
                start: q(16.0),
                end: q(20.0),
                chord: Chord::new(9, Quality::Minor),
            },
            ChordEvent {
                start: q(20.0),
                end: q(24.0),
                chord: Chord::new(5, Quality::Major),
            },
        ],
    }))
    .unwrap();
    let p = paint(&mut view, &s);
    let texts = p.texts();
    for t in [
        "Key",
        "Chords",
        "C Major",
        "A Minor",
        "Am",
        "F",
        "Key: C Major",
    ] {
        assert!(texts.contains(&t), "{t}: {texts:?}");
    }
    assert!(p.balanced_clips());
    // The strip pushes the grid down.
    assert_eq!(view.grid_top(), plain + KEY_ROW_H + CHORD_ROW_H);
    // The scale menu follows the key track (and can stop).
    let l = view.layout(SIZE);
    let items = view.toolbar_items(l.toolbar, &s);
    let at = items
        .iter()
        .find(|i| i.0 == crate::toolbar::Item::Scale)
        .unwrap()
        .1
        .center();
    let (_, req) = run(&mut view, down(at, Modifiers::NONE, 1), &mut s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("scale menu")
    };
    let follow = items
        .iter()
        .find(|i| i.label == "Follow the Key Track")
        .unwrap();
    assert_eq!(follow.checked, Some(true));
    // Fold to the scale: the clip's keys' notes (C major and A minor: the
    // same seven) and nothing else.
    let mut pr = s.editor.piano;
    pr.fold = KeyFold::Scale;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    let c_major = [0u8, 2, 4, 5, 7, 9, 11];
    let used: std::collections::HashSet<u8> = notes(&s).iter().map(|n| n.key).collect();
    assert!(
        view.rows
            .iter()
            .all(|k| c_major.contains(&(k % 12)) || used.contains(k))
    );
}

#[test]
fn the_midi_tools_panel_picks_sets_previews_and_applies() {
    use faderframe_project::midi_tools::Tool as MidiTool;
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let before = notes(&s).len();
    // Open it from the toolbar.
    let l = view.layout(SIZE);
    let items = view.toolbar_items(l.toolbar, &s);
    let at = items
        .iter()
        .find(|i| i.0 == crate::toolbar::Item::MidiTools)
        .unwrap()
        .1
        .center();
    run(&mut view, down(at, Modifiers::NONE, 1), &mut s);
    assert_eq!(s.editor.piano.tool, Some(MidiTool::Strum));
    let p = paint(&mut view, &s);
    assert!(p.texts().contains(&"MIDI TOOLS"));
    assert!(
        p.texts()
            .iter()
            .any(|t| t.starts_with("All ") && t.contains("notes →"))
    );
    // The grid made room for it.
    let l = view.layout(SIZE);
    assert!(l.tools.w > 200.0 && l.grid.right() <= l.tools.x);
    // Pick Chop, drag its parts up from 4 to 6.
    let g = view.tools_geometry(l.tools, &s).unwrap();
    let chop = g
        .tools
        .iter()
        .find(|(t, _)| *t == MidiTool::Chop)
        .unwrap()
        .1;
    run(&mut view, down(chop.center(), Modifiers::NONE, 1), &mut s);
    assert_eq!(s.editor.piano.tool, Some(MidiTool::Chop));
    let g = view.tools_geometry(l.tools, &s).unwrap();
    let parts = g.rows[0];
    let from = Point::new(parts.right() - 20.0, parts.center().y);
    run(&mut view, down(from, Modifiers::NONE, 1), &mut s);
    run(
        &mut view,
        drag(Point::new(from.x, from.y - 14.0), Modifiers::NONE),
        &mut s,
    );
    run(
        &mut view,
        up(Point::new(from.x, from.y - 14.0), Modifiers::NONE),
        &mut s,
    );
    assert_eq!(s.editor.piano.tools.chop_parts, 6);
    // Enter applies it to every note (none selected): six parts each.
    run(
        &mut view,
        ViewEvent::Key {
            key: Key::Enter,
            modifiers: Modifiers::NONE,
        },
        &mut s,
    );
    assert_eq!(notes(&s).len(), before * 6);
    // The Generate tab; the close button.
    let g = view.tools_geometry(l.tools, &s).unwrap();
    run(
        &mut view,
        down(g.tabs[1].center(), Modifiers::NONE, 1),
        &mut s,
    );
    assert!(s.editor.piano.tool.unwrap().is_generator());
    let g = view.tools_geometry(l.tools, &s).unwrap();
    run(
        &mut view,
        down(g.close.center(), Modifiers::NONE, 1),
        &mut s,
    );
    assert_eq!(s.editor.piano.tool, None);
    paint(&mut view, &s);
    assert_eq!(view.layout(SIZE).tools.w, 0.0);
}

#[test]
fn the_keyboard_plays_the_key_under_the_pointer_as_a_real_one_does() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let y = row_y(&mut view, &s, 61); // C#4
    let keys = view.layout(SIZE).keys;
    let played = |view: &mut PianoRollView, s: &mut Session, pos: Point| {
        let (actions, _) = run(view, down(pos, Modifiers::NONE, 1), s);
        run(view, up(pos, Modifiers::NONE), s);
        actions.iter().find_map(|a| match a {
            Action::Audition { key, .. } => Some(*key),
            _ => None,
        })
    };
    let half = view.row_h / 4.0;
    // On the black key itself: C#.
    let on_black = Point::new(keys.x + keys.w * 0.3, y);
    assert_eq!(view.keyboard_key_at(on_black, keys), 61);
    assert_eq!(played(&mut view, &mut s, on_black), Some(61));
    // Beside it: the white keys meeting under it, D above the middle, C
    // below.
    let beside = keys.x + keys.w * 0.85;
    assert_eq!(
        played(&mut view, &mut s, Point::new(beside, y - half)),
        Some(62)
    );
    assert_eq!(
        played(&mut view, &mut s, Point::new(beside, y + half)),
        Some(60)
    );
    // A white key's own row is that key across the width.
    let e = row_y(&mut view, &s, 64);
    assert_eq!(view.keyboard_key_at(Point::new(keys.x + 4.0, e), keys), 64);
    assert_eq!(view.keyboard_key_at(Point::new(beside, e), keys), 64);
    // Folded to the used keys, a black key without its white neighbours
    // shown is the black key across its row.
    let mut pr = s.editor.piano;
    pr.fold = KeyFold::Used;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    paint(&mut view, &s);
    for (i, &k) in view.rows.clone().iter().enumerate() {
        let y = view.grid_top() + i as f32 * view.row_h - view.scroll_y + view.row_h / 2.0;
        let got = view.keyboard_key_at(Point::new(beside, y - half), keys);
        assert!(view.rows.contains(&got));
        if is_black(k) && !view.rows.contains(&(k + 1)) {
            assert_eq!(got, k);
        }
    }
}

/// Shift-click on a note selects the notes from the one last clicked to it:
/// the keys between theirs, over the time they span.
#[test]
fn shift_click_selects_the_notes_between() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    let all = notes(&s);
    let a = all[0];
    let b = *all
        .iter()
        .find(|n| n.start > a.start && n.key.abs_diff(a.key) <= 4 && n.key != a.key)
        .expect("a later note near the first");
    let ya = row_y(&mut view, &s, a.key);
    let xa = view.x_of(a.start) + 9.0;
    run(
        &mut view,
        down(Point::new(xa, ya), Modifiers::NONE, 1),
        &mut s,
    );
    run(&mut view, up(Point::new(xa, ya), Modifiers::NONE), &mut s);
    let yb = view.y_of(b.key).unwrap() + view.row_h / 2.0;
    let xb = view.x_of(b.start) + 9.0;
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    run(&mut view, down(Point::new(xb, yb), shift, 1), &mut s);
    run(&mut view, up(Point::new(xb, yb), shift), &mut s);
    let (k0, k1) = (a.key.min(b.key), a.key.max(b.key));
    let (t0, t1) = (a.start, a.end().max(b.end()));
    let want: std::collections::BTreeSet<_> = all
        .iter()
        .filter(|n| (k0..=k1).contains(&n.key) && n.start < t1 && n.end() > t0)
        .map(|n| n.id)
        .collect();
    assert!(want.len() >= 2);
    assert_eq!(s.selection.notes, want);
}

/// Zoom like the arranger: the toolbar's −/+/Sel/Fit, Alt+wheel for the
/// row height.
#[test]
fn the_toolbar_zooms_and_alt_wheel_sizes_the_rows() {
    use crate::toolbar::{Item, Zoom};
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let l = view.layout(SIZE);
    let press = |view: &mut PianoRollView, s: &mut Session, z: Zoom| {
        let items = view.toolbar_items(view.layout(SIZE).toolbar, s);
        let at = items
            .iter()
            .find(|i| i.0 == Item::Zoom(z))
            .unwrap()
            .1
            .center();
        run(view, down(at, Modifiers::NONE, 1), s);
        run(view, up(at, Modifiers::NONE), s);
        paint(view, s);
    };
    let ppq = view.ppq;
    press(&mut view, &mut s, Zoom::In);
    assert!(view.ppq > ppq * 1.5, "in");
    press(&mut view, &mut s, Zoom::Out);
    assert!((view.ppq - ppq).abs() < 0.01, "and out again");
    // Fit: the clip's length across the grid, its notes' keys in view.
    press(&mut view, &mut s, Zoom::Fit);
    let len = notes(&s).iter().map(|n| n.end()).max().unwrap();
    let m_len = {
        let id = s.editor_clip().unwrap();
        s.project().clip(id).unwrap().as_midi().unwrap().length
    };
    assert!(view.x_of(m_len) <= l.grid.right() + 1.0, "the clip fits");
    assert!(view.x_of(len) > l.grid.x);
    for n in notes(&s) {
        let y = view.y_of(n.key).unwrap();
        assert!(
            y >= l.grid.y - 1.0 && y <= l.grid.bottom(),
            "{} shown",
            n.key
        );
    }
    // Alt+wheel: smaller rows, then taller again.
    let wheel = |view: &mut PianoRollView, s: &mut Session, dy: f32| {
        run(
            view,
            ViewEvent::Scroll {
                pos: l.grid.center(),
                dx: 0.0,
                dy,
                modifiers: Modifiers {
                    alt: true,
                    ..Modifiers::NONE
                },
                precise: false,
            },
            s,
        );
    };
    let row_h = view.row_h;
    wheel(&mut view, &mut s, 2.0);
    let smaller = view.row_h;
    assert!(smaller < row_h, "{smaller} < {row_h}");
    wheel(&mut view, &mut s, -1.0);
    assert!(view.row_h > smaller);
}

/// Snap to Objects in the piano roll: a note moved (grid off) to a few
/// pixels from another note's start lands on it, with a guide naming it;
/// Shift moves freely; a resized end snaps to the next note's start.
#[test]
fn notes_snap_to_other_notes_and_objects() {
    let mut s = session();
    s.dispatch(Action::SetEditMode(faderframe_session::EditMode::Slip))
        .unwrap();
    let mut view = PianoRollView::new(Theme::default());
    let all = notes(&s);
    let n = all[0];
    // Another note starting later than `n` ends.
    let o = *all
        .iter()
        .filter(|o| o.id != n.id && o.start > n.end() + MusicalTime::QUARTER)
        .min_by_key(|o| o.start)
        .unwrap();
    let y = row_y(&mut view, &s, n.key);
    let grab = Point::new(view.x_of(n.start) + 9.0, y);
    let press = |view: &mut PianoRollView, s: &mut Session, m: Modifiers| {
        run(view, down(grab, m, 1), s);
        // Its start 3 px past `o`'s.
        let to = Point::new(view.x_of(o.start) + 9.0 + 3.0, y);
        run(view, drag(Point::new(grab.x + 20.0, y), m), s);
        run(view, drag(to, m), s);
        let guide = view.snap_guide;
        run(view, up(to, m), s);
        assert_eq!(view.snap_guide, None, "the guide goes with the drag");
        guide
    };
    let guide = press(&mut view, &mut s, Modifiers::NONE);
    let moved = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    assert_eq!(moved.start, o.start, "onto the other note's start");
    assert_eq!(
        guide.map(|g| g.object),
        Some(faderframe_session::SnapObject::NoteStart(o.id))
    );
    let mut painted = RecordingPainter::new();
    s.dispatch(Action::Undo).unwrap();
    // Shift: free, 3 px past.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    assert_eq!(press(&mut view, &mut s, shift), None);
    let moved = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    let px = (moved.start - o.start).quarters() * f64::from(view.ppq);
    assert!((px - 3.0).abs() < 0.5, "{px}");
    s.dispatch(Action::Undo).unwrap();
    // A resize of `n`'s end to 4 px short of `o`'s start lands on the
    // note edge nearest the pointer (`o`'s start or another note's end).
    let end_x = view.x_of(n.end()) - 1.0;
    run(
        &mut view,
        down(Point::new(end_x, y), Modifiers::NONE, 1),
        &mut s,
    );
    let to = Point::new(view.x_of(o.start) - 4.0, y);
    run(&mut view, drag(to, Modifiers::NONE), &mut s);
    let hit = view.snap_guide.expect("snapped");
    assert!(matches!(
        hit.object,
        faderframe_session::SnapObject::NoteStart(_) | faderframe_session::SnapObject::NoteEnd(_)
    ));
    let clip_start = s.project().clip(s.editor_clip().unwrap()).unwrap().start;
    let pointer = clip_start + view.time_at(to.x);
    let px = |t: MusicalTime| ((t - pointer).quarters() * f64::from(view.ppq)).abs();
    assert!(px(hit.at) <= px(clip_start + o.start) + 1e-3, "the nearest");
    view.paint(&mut painted, SIZE, &s, &Theme::default());
    let texts = painted.texts();
    assert!(
        texts.contains(&"Note Start") || texts.contains(&"Note End"),
        "the guide is named: {texts:?}"
    );
    run(&mut view, up(to, Modifiers::NONE), &mut s);
    let resized = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    assert_eq!(clip_start + resized.end(), hit.at);
}

/// Drum clips show diamonds on the notes' starts (issue #28): on by
/// themselves for drums (here: every note on MIDI channel 10), or chosen;
/// a diamond is grabbed where the note starts and moves it whole (no
/// length edges).
#[test]
fn drum_notes_are_diamonds_grabbed_at_their_start() {
    use faderframe_session::NoteStyle;
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    assert!(!view.diamonds, "the demo's melody: bars");
    let clip = s.editor_clip().unwrap();
    s.dispatch(Action::SetMidiChannel {
        clips: vec![clip],
        channel: 9,
    })
    .unwrap();
    paint(&mut view, &s);
    assert!(view.diamonds, "channel 10: drums");
    let bars = PianoRollSettings {
        note_style: NoteStyle::Bars,
        ..s.editor.piano
    };
    s.dispatch(Action::SetPianoRoll(bars)).unwrap();
    paint(&mut view, &s);
    assert!(!view.diamonds, "bars chosen");
    s.dispatch(Action::SetPianoRoll(PianoRollSettings {
        note_style: NoteStyle::Diamonds,
        ..bars
    }))
    .unwrap();
    let n = notes(&s)[0];
    let y = row_y(&mut view, &s, n.key);
    assert!(view.diamonds);
    let r = view.note_rect(&n).unwrap();
    assert!(
        (r.center().x - view.x_of(n.start)).abs() < 0.01,
        "on its start"
    );
    assert!(r.w < view.row_h * 2.0, "{r:?}");
    let p = paint(&mut view, &s);
    assert!(
        p.ops
            .iter()
            .any(|o| matches!(o, faderframe_ui_canvas::DrawOp::Path)),
        "drawn as paths"
    );
    // Its end is no edge to grab: a press on the diamond moves the note.
    view.set_tool(Tool::Pointer);
    let x = view.x_of(n.start) + r.w * 0.3;
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    let to = view.x_of(n.start + MusicalTime::from_quarters_i(1)) + r.w * 0.3;
    run(&mut view, drag(Point::new(to, y), Modifiers::NONE), &mut s);
    run(&mut view, up(Point::new(to, y), Modifiers::NONE), &mut s);
    let moved = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    assert_eq!(moved.start, n.start + MusicalTime::from_quarters_i(1));
    assert_eq!(moved.length, n.length, "its length kept");
}

/// A note locked to time: the note menu locks it (checked once locked),
/// it shows a padlock at its start, and the inspector, tooltip and
/// accessible name say so.
#[test]
fn a_note_locked_to_time_shows_a_padlock_and_its_menu_locks_it() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    view.set_tool(Tool::Pointer);
    let n = notes(&s)[0];
    let y = row_y(&mut view, &s, n.key);
    let r = view.note_rect(&n).unwrap();
    let pos = Point::new(r.x + r.w * 0.6, y);
    let menu = |view: &mut PianoRollView, s: &mut Session| {
        let (_, requests) = run(
            view,
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            s,
        );
        let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
            panic!("the note menu")
        };
        items
            .into_iter()
            .find(|i| i.label == "Lock to Time")
            .expect("Lock to Time")
    };
    let padlock = |p: &RecordingPainter, at: Rect| {
        p.ops.iter().any(
            |o| matches!(o, faderframe_ui_canvas::DrawOp::Rounded(g) if at.contains(g.center())),
        )
    };
    let glyph = crate::paint::lock_rect(r, false).unwrap();
    assert!(!padlock(&paint(&mut view, &s), glyph), "none while musical");
    let lock = menu(&mut view, &mut s);
    assert_eq!(lock.checked, Some(false));
    s.dispatch(lock.action.unwrap()).unwrap();
    let locked = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    assert!(locked.time_locked);
    let r = view.note_rect(&locked).unwrap();
    let glyph = crate::paint::lock_rect(r, false).unwrap();
    assert!(
        padlock(&paint(&mut view, &s), glyph),
        "a padlock at its start"
    );
    assert!(
        view.inspector_text(&s).contains("locked at"),
        "{}",
        view.inspector_text(&s)
    );
    let tip = view.tooltip_at(pos, SIZE, &s).unwrap();
    assert!(tip.contains("locked to time at"), "{tip}");
    assert!(
        view.accessible(SIZE, &s)
            .iter()
            .flat_map(|a| a.children.iter())
            .any(|a| a.label.contains("locked to time"))
    );
    // Checked now; again: free.
    let unlock = menu(&mut view, &mut s);
    assert_eq!(unlock.checked, Some(true));
    s.dispatch(unlock.action.unwrap()).unwrap();
    assert!(!notes(&s).into_iter().any(|m| m.id == n.id && m.time_locked));
}

/// With a drum map, drum mode lists the map's sounds by name in its
/// order, and drawing on a sound with a grid of its own keeps to it.
#[test]
fn a_drum_map_lists_its_sounds_by_name_and_draws_on_their_grids() {
    use faderframe_session::maps::MapOp;
    let mut s = session();
    let clip = s.editor_clip().unwrap();
    let track = s.project().clip(clip).unwrap().track;
    s.use_drum_map_named(track, "gm").unwrap();
    let mut view = PianoRollView::new(Theme::default());
    let p = paint(&mut view, &s);
    assert!(view.diamonds, "a drum map makes a drum track");
    assert!(view.drum_rows);
    // Kick, kick, snare … as the map lists them, top to bottom.
    assert_eq!(&view.rows[..3], &[36, 35, 38]);
    assert_eq!(view.row_of(38), Some(2));
    assert!(view.y_of(36).unwrap() < view.y_of(35).unwrap());
    assert!(p.texts().contains(&"Bass Drum 1"), "{:?}", p.texts());
    // The closed hi-hat on a triplet grid of its own.
    let mut map = s.project().track_drum_map(track).unwrap().clone();
    map.sound_mut(42).unwrap().quantize = Some(faderframe_timeline::GridDivision::Triplet(8));
    s.dispatch(Action::Maps(MapOp::EditDrumMap(Box::new(map))))
        .unwrap();
    let y = row_y(&mut view, &s, 42);
    assert_eq!(view.key_at(y), 42);
    view.set_tool(Tool::Pencil);
    let before = notes(&s).len();
    let x = view.x_of(MusicalTime::from_quarters(0.45));
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    run(&mut view, up(Point::new(x, y), Modifiers::NONE), &mut s);
    let drawn: Vec<MidiNote> = notes(&s).into_iter().filter(|n| n.key == 42).collect();
    assert_eq!(notes(&s).len(), before + 1);
    let third = MusicalTime::from_quarters(1.0 / 3.0);
    assert!(
        drawn
            .iter()
            .any(|n| (n.start.ticks() - third.ticks()).abs() <= 1
                && (n.length.ticks() - third.ticks()).abs() <= 1),
        "on the triplet eighth: {drawn:?}"
    );
    // The names say where a sound goes.
    let tip = view
        .tooltip(Point::new(view.layout(SIZE).keys.x + 10.0, y), SIZE, &s)
        .unwrap();
    assert!(tip.starts_with("Closed Hi-Hat"), "{tip}");
}

/// The articulation lane: chosen from the lane menu, the map's
/// articulations by name, a click sets a direction (or removes it), a
/// click under a note gives it an attribute; notes' menus offer them.
#[test]
fn the_articulation_lane_sets_directions_and_attributes() {
    use faderframe_project::articulation::ArticulationKind;
    let mut s = session();
    let clip = s.editor_clip().unwrap();
    let track = s.project().clip(clip).unwrap().track;
    s.use_articulation_map_named(track, "strings").unwrap();
    let map = s.project().track_articulation_map(track).unwrap().clone();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let l = view.layout(SIZE);
    let (_, requests) = run(
        &mut view,
        down(l.lane_header.center(), Modifiers::NONE, 1),
        &mut s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("the lane menu");
    };
    let item = items.iter().find(|i| i.label == "Articulations").unwrap();
    s.dispatch(item.action.clone().unwrap()).unwrap();
    assert!(s.editor.piano.articulations);
    let p = paint(&mut view, &s);
    assert_eq!(view.lane(), LaneKind::Articulation);
    assert!(p.texts().contains(&"→ Pizzicato"), "{:?}", p.texts());
    assert!(p.texts().contains(&"• Staccato"));
    let l = view.layout(SIZE);
    let area = PianoRollView::lane_value_rect(l.lane);
    let n = map.articulations.len();
    let row_of = |name: &str| {
        map.articulations
            .iter()
            .position(|a| a.name == name)
            .unwrap()
    };
    // A direction where the pointer is (on the grid).
    let pizz = row_of("Pizzicato");
    let y = PianoRollView::articulation_row(area, pizz, n).center().y;
    let x = view.x_of(MusicalTime::from_quarters(2.0));
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    let lane = |s: &Session| {
        s.project()
            .clip(clip)
            .unwrap()
            .as_midi()
            .unwrap()
            .articulations
            .clone()
    };
    assert_eq!(lane(&s).len(), 1);
    assert_eq!(lane(&s)[0].time, MusicalTime::from_quarters(2.0));
    assert_eq!(lane(&s)[0].articulation, map.articulations[pizz].id);
    // Clicked again: gone.
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    assert!(lane(&s).is_empty());
    // An attribute under the first note.
    let first = notes(&s)[0];
    let stacc = row_of("Staccato");
    assert_eq!(map.articulations[stacc].kind, ArticulationKind::Attribute);
    let y = PianoRollView::articulation_row(area, stacc, n).center().y;
    let x = view.x_of(first.start + MusicalTime(first.length.ticks() / 2));
    run(
        &mut view,
        down(Point::new(x, y), Modifiers::NONE, 1),
        &mut s,
    );
    let marked = notes(&s).into_iter().find(|m| m.id == first.id).unwrap();
    assert_eq!(marked.articulation, Some(map.articulations[stacc].id));
    // Shown on the note, by its symbol.
    let p = paint(&mut view, &s);
    assert!(
        p.texts().iter().any(|t| t.ends_with("stacc."))
            || view.note_rect(&marked).is_none_or(|r| r.w <= 28.0),
        "{:?}",
        p.texts()
    );
    // A note's menu offers the articulations.
    let y = row_y(&mut view, &s, first.key);
    let x = view.x_of(first.start) + 2.0;
    let (_, requests) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: Point::new(x, y),
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &mut s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("the note menu");
    };
    let sub = items.iter().find(|i| i.label == "Articulation").unwrap();
    assert!(
        sub.children
            .iter()
            .any(|c| c.label == "Staccato" && c.checked == Some(true))
    );
    // The lane's menu opens the map in the map editor.
    let (_, requests) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: area.center(),
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &mut s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("the lane's menu");
    };
    let edit = items
        .iter()
        .find(|i| i.label == "Edit Articulation Map…")
        .unwrap();
    s.dispatch(edit.action.clone().unwrap()).unwrap();
    assert_eq!(
        s.map_editor(),
        Some(faderframe_session::maps::MapRef::Articulation(map.id))
    );
}

/// The velocity lane drags at MIDI 2.0's 16 bits: a few pixels move a
/// velocity by less than a MIDI 1.0 step, and the inspector shows the
/// fraction.
#[test]
fn velocity_drags_are_at_16_bits() {
    let mut s = session();
    let mut view = PianoRollView::new(Theme::default());
    paint(&mut view, &s);
    let n = notes(&s)[0];
    let l = view.layout(SIZE);
    let area = PianoRollView::lane_value_rect(l.lane);
    let x = view.x_of(n.start);
    let y0 = area.bottom() - area.h * n.velocity_unit() as f32;
    let to = Point::new(x, y0 - area.h / 300.0);
    run(
        &mut view,
        down(Point::new(x, y0), Modifiers::NONE, 1),
        &mut s,
    );
    run(&mut view, drag(to, Modifiers::NONE), &mut s);
    run(&mut view, up(to, Modifiers::NONE), &mut s);
    let moved = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    let by = i32::from(moved.velocity16()) - i32::from(n.velocity16());
    assert!(by > 0 && by < 512, "moved by {by} of 65535");
    assert!(moved.has_fine_velocity());
    assert!(
        moved.velocity_text().contains('.'),
        "{}",
        moved.velocity_text()
    );
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    let back = notes(&s).into_iter().find(|m| m.id == n.id).unwrap();
    assert_eq!(back.velocity16(), n.velocity16());
}
