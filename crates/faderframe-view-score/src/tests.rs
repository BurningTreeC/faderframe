#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_leadsheet::score::Clef;
use faderframe_project::{Clip, ClipContent, Command, MidiClip, MidiNote, Project, TrackKind};
use faderframe_session::NoteOp;
use faderframe_session::score::ScoreOf;
use faderframe_ui_canvas::{DrawOp, RecordingPainter};

const SIZE: Size = Size::new(1200.0, 800.0);

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

fn note(id: u64, start: f64, length: f64, key: u8) -> MidiNote {
    MidiNote {
        id: NoteId(id),
        start: q(start),
        length: q(length),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
        articulation: None,
        fine: None,
    }
}

fn session(bars: f64, notes: Vec<MidiNote>) -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Score", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    let id = ClipId(81_001);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "tune".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: q(4.0 * bars),
                notes,
                ..MidiClip::default()
            }),
        }),
    })
    .unwrap();
    s.dispatch(Action::Score(ScoreOp::Show(ScoreOf::Clip(id))))
        .unwrap();
    (s, t, id)
}

fn four() -> Vec<MidiNote> {
    vec![
        note(1, 0.0, 1.0, 60),
        note(2, 1.0, 1.0, 64),
        note(3, 2.0, 1.0, 67),
        note(4, 3.0, 1.0, 72),
    ]
}

fn paint(view: &mut ScoreView, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p
}

fn run(
    view: &mut ScoreView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn apply(s: &mut Session, actions: Vec<Action>) {
    for a in actions {
        s.dispatch(a).unwrap();
    }
}

fn click(pos: Point, button: PointerButton, modifiers: Modifiers) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button,
        modifiers,
        clicks: 1,
    }
}

fn key(c: char, modifiers: Modifiers) -> ViewEvent {
    ViewEvent::Key {
        key: Key::Char(c),
        modifiers,
    }
}

/// Where a note's head is drawn in the view.
fn head_of(view: &ScoreView, s: &Session, note: NoteId) -> Point {
    let doc = s.score().unwrap();
    for (part, bars) in doc.map.events.iter().enumerate() {
        for (bar, events) in bars.iter().enumerate() {
            let m = &doc.sheet.parts[part].measures[bar];
            for (b, e) in events.iter().zip(m.all_events()) {
                let Some(b) = b else { continue };
                if let Some(i) = e.sources.iter().position(|x| *x == note.raw()) {
                    let h = b.heads[i];
                    return view
                        .to_view(
                            b.page,
                            Point::new(h.x, h.y),
                            SIZE,
                            &doc,
                            s.score_settings().layout,
                        )
                        .unwrap();
                }
            }
        }
    }
    panic!("note {note:?} not drawn");
}

fn entering(s: &mut Session) {
    let settings = ScoreSettings {
        entering: true,
        ..s.score_settings()
    };
    s.dispatch(Action::Score(ScoreOp::Settings(settings)))
        .unwrap();
}

fn keys_of(s: &Session, c: ClipId) -> Vec<(f64, u8)> {
    let mut out: Vec<(f64, u8)> = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start.quarters(), n.key))
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    out
}

#[test]
fn a_click_on_a_notehead_selects_its_note_and_puts_the_cursor_there() {
    let (mut s, _, _) = session(1.0, four());
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let at = head_of(&view, &s, NoteId(2));
    let (actions, _) = run(
        &mut view,
        click(at, PointerButton::Primary, Modifiers::NONE),
        &s,
    );
    assert_eq!(
        actions,
        vec![Action::Several(vec![
            Action::SelectNotes {
                notes: vec![NoteId(2)],
                mode: SelectMode::Replace
            },
            Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
                part: 0,
                at: q(1.0)
            }))),
        ])]
    );
    apply(&mut s, actions);
    // Shift adds the next.
    let at = head_of(&view, &s, NoteId(3));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let (actions, _) = run(&mut view, click(at, PointerButton::Primary, shift), &s);
    apply(&mut s, actions);
    assert_eq!(
        s.selection.notes.iter().copied().collect::<Vec<_>>(),
        vec![NoteId(2), NoteId(3)]
    );
    // Selected heads are drawn again in the accent.
    let paths = |view: &mut ScoreView, s: &Session| {
        paint(view, s)
            .ops
            .iter()
            .filter(|o| matches!(o, DrawOp::Path))
            .count()
    };
    let marked = paths(&mut view, &s);
    s.dispatch(Action::SelectNotes {
        notes: Vec::new(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    let plain = paths(&mut view, &s);
    assert_eq!(marked, plain + 2);
}

/// In note input a click on the staff enters a note there: the line under
/// the pointer, the time snapped to the value.
#[test]
fn note_input_clicks_enter_notes_where_the_pointer_is() {
    let (mut s, _, c) = session(1.0, Vec::new());
    entering(&mut s);
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let doc = s.score().unwrap();
    let sp = doc.map.space;
    // Beat 3 of the bar, G4's line (the treble staff's second).
    let (page, x, staff, _) = doc.map.x_of(0, 0, 24.0).unwrap();
    let y = staff + 3.0 * sp;
    let at = view
        .to_view(
            page,
            Point::new(x + 0.3 * sp, y),
            SIZE,
            &doc,
            ScoreLayout::Pages,
        )
        .unwrap();
    let (actions, _) = run(
        &mut view,
        click(at, PointerButton::Primary, Modifiers::NONE),
        &s,
    );
    assert_eq!(
        actions,
        vec![Action::Score(ScoreOp::EnterAt {
            part: 0,
            at: q(2.0),
            diatonic: 32
        })]
    );
    apply(&mut s, actions);
    assert_eq!(keys_of(&s, c), vec![(2.0, 67)]);
    assert_eq!(diatonic_at(2, Clef::Treble), 32);
}

/// Typed keys are score keys: letters enter at the cursor, Shift adds to
/// the chord, digits choose values; arrows transpose the selection.
#[test]
fn typed_keys_enter_notes() {
    let (mut s, _, c) = session(1.0, Vec::new());
    entering(&mut s);
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let mut all = Vec::new();
    for (ch, m) in [
        ('5', Modifiers::NONE),
        ('c', Modifiers::NONE),
        ('e', shift),
        ('4', Modifiers::NONE),
        ('d', Modifiers::NONE),
        ('e', Modifiers::NONE),
    ] {
        let (actions, _) = run(&mut view, key(ch, m), &s);
        assert_eq!(actions.len(), 1, "{ch}");
        all.extend(actions.clone());
        apply(&mut s, actions);
    }
    assert_eq!(
        all[..3],
        [
            Action::Score(ScoreOp::Key(ScoreKey::Value(Value::Quarter))),
            Action::Score(ScoreOp::Key(ScoreKey::Letter {
                step: 0,
                chord: false
            })),
            Action::Score(ScoreOp::Key(ScoreKey::Letter {
                step: 2,
                chord: true
            })),
        ]
    );
    assert_eq!(
        keys_of(&s, c),
        vec![(0.0, 72), (0.0, 76), (1.0, 74), (1.5, 76)]
    );
    // ↑ moves the note entered last a semitone (spelt sharp).
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Up,
            modifiers: Modifiers::NONE,
        },
        &s,
    );
    assert_eq!(
        actions,
        vec![Action::Score(ScoreOp::Key(ScoreKey::Transpose(
            Interval::Semitones(1)
        )))]
    );
    apply(&mut s, actions);
    assert_eq!(keys_of(&s, c).last(), Some(&(1.5, 77)));
}

#[test]
fn the_palette_chooses_values_and_types_words() {
    let (mut s, _, _) = session(1.0, four());
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let rect = |item: Item| {
        ScoreView::palette(SIZE)
            .into_iter()
            .find(|(i, _)| *i == item)
            .unwrap()
            .1
    };
    let (actions, _) = run(
        &mut view,
        click(
            rect(Item::Value(Value::Eighth)).center(),
            PointerButton::Primary,
            Modifiers::NONE,
        ),
        &s,
    );
    assert_eq!(
        actions,
        vec![Action::Score(ScoreOp::Key(ScoreKey::Value(Value::Eighth)))]
    );
    apply(&mut s, actions);
    assert_eq!(s.score_settings().value, Value::Eighth);
    // Words from the selected note on.
    s.dispatch(Action::SelectNotes {
        notes: vec![NoteId(1)],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let (_, requests) = run(
        &mut view,
        click(
            rect(Item::Lyrics).center(),
            PointerButton::Primary,
            Modifiers::NONE,
        ),
        &s,
    );
    let Some(HostRequest::TextInput { commit, .. }) = requests.into_iter().next() else {
        panic!("a text entry");
    };
    let a = commit("la la-la").unwrap();
    s.dispatch(a).unwrap();
    let doc = s.score().unwrap();
    let words: Vec<String> = doc.sheet.parts[0].measures[0]
        .events
        .iter()
        .filter_map(|e| e.lyric.as_ref().map(|l| l.text.clone()))
        .collect();
    assert_eq!(words, vec!["la", "la", "la"]);
    // A chord symbol at the selected note (Ctrl+K).
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let (_, requests) = run(&mut view, key('k', ctrl), &s);
    let Some(HostRequest::TextInput { commit, .. }) = requests.into_iter().next() else {
        panic!("a text entry");
    };
    s.dispatch(commit("Cmaj7").unwrap()).unwrap();
    assert_eq!(s.project().chords.len(), 1);
}

/// A note's menu: its dynamic is played as velocity.
#[test]
fn a_notes_menu_changes_it() {
    let (mut s, _, c) = session(1.0, four());
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let at = head_of(&view, &s, NoteId(3));
    let (actions, requests) = run(
        &mut view,
        click(at, PointerButton::Secondary, Modifiers::NONE),
        &s,
    );
    apply(&mut s, actions);
    let Some(items) = requests.into_iter().find_map(|r| match r {
        HostRequest::ContextMenu { items, .. } => Some(items),
        _ => None,
    }) else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|m| m.label.as_str()).collect();
    for l in [
        "Duration",
        "Transpose",
        "Accidental",
        "Voice",
        "Dynamic",
        "Articulation",
        "Delete",
    ] {
        assert!(labels.contains(&l), "{l} in {labels:?}");
    }
    let dynamic = items.into_iter().find(|m| m.label == "Dynamic").unwrap();
    let mf = dynamic
        .children
        .into_iter()
        .find(|m| m.label == "mf")
        .unwrap();
    s.dispatch(mf.action.unwrap()).unwrap();
    let m = s.project().clip(c).unwrap().as_midi().unwrap();
    assert_eq!(m.note(NoteId(3)).unwrap().velocity, 80);
    assert_eq!(m.note(NoteId(4)).unwrap().velocity, 80);
    assert_eq!(m.note(NoteId(2)).unwrap().velocity, 100);
}

/// The piano roll's edits are the score's: a note moved an octave up is
/// drawn higher.
#[test]
fn a_piano_roll_edit_is_redrawn_in_the_score() {
    let (mut s, _, c) = session(1.0, four());
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let before = head_of(&view, &s, NoteId(1));
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![NoteId(1)],
        op: NoteOp::Transpose(12),
    })
    .unwrap();
    paint(&mut view, &s);
    let after = head_of(&view, &s, NoteId(1));
    assert!(after.y < before.y - 10.0, "{before:?} → {after:?}");
    // Delete in the score: gone from the piano roll's clip.
    s.dispatch(Action::SelectNotes {
        notes: vec![NoteId(2)],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Delete,
            modifiers: Modifiers::NONE,
        },
        &s,
    );
    apply(&mut s, actions);
    assert_eq!(keys_of(&s, c).len(), 3);
}

/// A strip scrolls sideways and its notes are still found.
#[test]
fn the_strip_scrolls_sideways() {
    let notes: Vec<MidiNote> = (0..64)
        .map(|i| note(100 + i, i as f64 * 0.5, 0.5, 60 + (i % 12) as u8))
        .collect();
    let (mut s, _, _) = session(8.0, notes);
    let settings = ScoreSettings {
        layout: ScoreLayout::Strip,
        ..s.score_settings()
    };
    s.dispatch(Action::Score(ScoreOp::Settings(settings)))
        .unwrap();
    let mut view = ScoreView::new(Theme::default());
    paint(&mut view, &s);
    let info = view.scroll_info(ScrollAxis::Horizontal, SIZE, &s).unwrap();
    assert!(info.content > info.viewport, "{info:?}");
    let first = head_of(&view, &s, NoteId(100));
    let (_, _) = run(
        &mut view,
        ViewEvent::Scroll {
            pos: Point::new(600.0, 400.0),
            dx: 0.0,
            dy: 3.0,
            modifiers: Modifiers::NONE,
            precise: false,
        },
        &s,
    );
    let moved = head_of(&view, &s, NoteId(100));
    assert!(moved.x < first.x - 100.0, "{first:?} → {moved:?}");
    let doc = s.score().unwrap();
    let last = head_of(&view, &s, NoteId(163));
    if last.x < SIZE.w {
        assert!(matches!(
            view.pointing(last, SIZE, &doc, ScoreLayout::Strip),
            Some(Pointing::Note(n, _)) if n == NoteId(163)
        ));
    }
}

#[test]
fn without_a_clip_it_says_how_to_get_one() {
    let s = Session::new(Project::new("Empty", 48_000), None, EngineConfig::default()).unwrap();
    let mut view = ScoreView::new(Theme::default());
    let p = paint(&mut view, &s);
    assert!(p.texts().iter().any(|t| t.starts_with("No score yet")));
    assert!(p.balanced_clips());
}
