#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::TrackKind;
use faderframe_project::articulation::{ArticulationKind, ControlValue, Keyswitch};
use faderframe_session::maps::DrumMapSource;
use faderframe_timeline::GridDivision;
use faderframe_ui_canvas::{
    AccessNode, CanvasView, Modifiers, RecordingPainter, access::find_node, access::press_key,
};

const SIZE: Size = Size {
    w: 1300.0,
    h: 640.0,
};

/// The demo with the Strings template on its lead synth, shown.
fn session() -> (Session, TrackId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap()
        .id;
    s.dispatch(Action::Maps(MapOp::NewArticulationMap {
        track: Some(t),
        template: Some("Strings".into()),
    }))
    .unwrap();
    (s, t)
}

/// The same with the General MIDI drum map shown.
fn drums() -> (Session, TrackId) {
    let (mut s, t) = session();
    s.dispatch(Action::Maps(MapOp::NewDrumMap {
        track: Some(t),
        from: DrumMapSource::Gm,
    }))
    .unwrap();
    (s, t)
}

fn art(s: &Session) -> ArticulationMap {
    match MapsView::doc(s).unwrap() {
        Doc::Art(m) => m.clone(),
        Doc::Drum(_) => panic!("an articulation map"),
    }
}

fn drum(s: &Session) -> DrumMap {
    match MapsView::doc(s).unwrap() {
        Doc::Drum(m) => m.clone(),
        Doc::Art(_) => panic!("a drum map"),
    }
}

/// Run one event; the actions and host requests it made.
fn run(view: &mut MapsView, ev: ViewEvent, s: &Session) -> (Vec<Action>, Vec<HostRequest<Action>>) {
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

fn up(pos: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

fn moved(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn key(k: Key, modifiers: Modifiers) -> ViewEvent {
    ViewEvent::Key { key: k, modifiers }
}

fn apply(s: &mut Session, actions: Vec<Action>) {
    for a in actions {
        s.dispatch(a).unwrap();
    }
}

fn paint(view: &mut MapsView, s: &Session) -> Vec<String> {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p.texts().into_iter().map(str::to_string).collect()
}

/// The middle of cell `col` of the `i`th row shown.
fn cell(view: &MapsView, s: &Session, i: usize, col: Col) -> Point {
    let doc = MapsView::doc(s).unwrap();
    let row = view.rows(doc)[i];
    view.cell(doc, Some(row), col, view.row_rect(i, SIZE))
        .center()
}

/// The one edit an event sequence made (and it applied).
fn one_edit(s: &mut Session, actions: Vec<Action>) {
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert!(
        matches!(
            actions[0],
            Action::Maps(MapOp::EditArticulationMap(_) | MapOp::EditDrumMap(_))
        ),
        "{actions:?}"
    );
    let before = s.history().undo_labels().len();
    apply(s, actions);
    assert_eq!(s.history().undo_labels().len(), before + 1, "one undo step");
}

/// Double-click a cell and type `text`: the action its entry makes.
fn type_into(view: &mut MapsView, s: &Session, i: usize, col: Col, text: &str) -> Option<Action> {
    let at = cell(view, s, i, col);
    let (actions, requests) = run(view, down(at, 2), s);
    assert!(actions.is_empty());
    let req = requests
        .into_iter()
        .find_map(|r| match r {
            HostRequest::TextInput { commit, .. } => Some(commit),
            _ => None,
        })
        .expect("a text entry");
    run(view, up(at), s);
    req(text)
}

/// Click a cell whose menu opens and choose `label` (` > ` goes into
/// submenus).
fn choose(view: &mut MapsView, s: &Session, i: usize, col: Col, label: &str) -> Action {
    let at = cell(view, s, i, col);
    let (_, requests) = run(view, down(at, 1), s);
    run(view, up(at), s);
    let items = requests
        .into_iter()
        .find_map(|r| match r {
            HostRequest::ContextMenu { items, .. } => Some(items),
            _ => None,
        })
        .expect("a menu");
    pick(&items, label)
}

fn pick(items: &[MenuItem<Action>], path: &str) -> Action {
    let mut items = items.to_vec();
    let parts: Vec<&str> = path.split(" > ").collect();
    for (k, part) in parts.iter().enumerate() {
        let item = items
            .iter()
            .find(|i| i.label == *part)
            .unwrap_or_else(|| {
                panic!(
                    "no {part} in {:?}",
                    items.iter().map(|i| &i.label).collect::<Vec<_>>()
                )
            })
            .clone();
        if k + 1 == parts.len() {
            return item.action.expect("an entry that acts");
        }
        items = item.children;
    }
    unreachable!()
}

fn press_button(
    view: &mut MapsView,
    s: &Session,
    b: Button,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let r = MapsView::buttons(SIZE, s)
        .into_iter()
        .find(|(x, _)| *x == b)
        .unwrap()
        .1;
    let out = run(view, down(r.center(), 1), s);
    run(view, up(r.center()), s);
    out
}

#[test]
fn names_keyswitches_controllers_and_numbers_are_typed_one_step_each() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t == "Strings ▾"));
    assert!(texts.iter().any(|t| t == "Sustain"));
    // The name.
    let a = type_into(&mut view, &s, 0, Col::Name, "Long").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[0].name, "Long");
    assert!(
        type_into(&mut view, &s, 0, Col::Name, "  ").is_none(),
        "no empty name"
    );
    // The same name again: nothing to do.
    assert!(type_into(&mut view, &s, 0, Col::Name, "Long").is_none());
    // Keyswitches with a velocity, controllers.
    let a = type_into(&mut view, &s, 1, Col::Keyswitches, "C0, D#0@90").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(
        art(&s).articulations[1].output.keyswitches,
        vec![
            Keyswitch {
                key: 12,
                velocity: 100
            },
            Keyswitch {
                key: 15,
                velocity: 90
            }
        ]
    );
    let a = type_into(&mut view, &s, 1, Col::Controls, "1=64, 68=127").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(
        art(&s).articulations[1].output.controls,
        vec![
            ControlValue {
                controller: 1,
                value: 64
            },
            ControlValue {
                controller: 68,
                value: 127
            }
        ]
    );
    assert!(type_into(&mut view, &s, 1, Col::Controls, "1=300").is_none());
    // A program (shown from 1), cleared again.
    let a = type_into(&mut view, &s, 2, Col::Program, "5").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[2].output.program, Some(4));
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t == "5"));
    let a = type_into(&mut view, &s, 2, Col::Program, "").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[2].output.program, None);
    // Transpose, velocity, length; out of range refused.
    let a = type_into(&mut view, &s, 3, Col::Transpose, "−12").unwrap();
    one_edit(&mut s, vec![a]);
    let a = type_into(&mut view, &s, 3, Col::Velocity, "80%").unwrap();
    one_edit(&mut s, vec![a]);
    let a = type_into(&mut view, &s, 3, Col::Length, "150").unwrap();
    one_edit(&mut s, vec![a]);
    assert!(type_into(&mut view, &s, 3, Col::Length, "0").is_none());
    let o = &art(&s).articulations[3].output;
    assert_eq!((o.transpose, o.velocity, o.length), (-12, 80, 150));
    // The symbol may be empty.
    let a = type_into(&mut view, &s, 3, Col::Symbol, "").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[3].symbol, "");
    // Each was a step of its own.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(art(&s).articulations[3].symbol, "trem.");
}

#[test]
fn numbers_drag_and_commit_once_on_release() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    let at = cell(&view, &s, 0, Col::Transpose);
    let (actions, _) = run(&mut view, down(at, 1), &s);
    assert!(actions.is_empty());
    let to = Point::new(at.x, at.y - 3.0 * DRAG_PX);
    let (actions, _) = run(&mut view, moved(to), &s);
    assert!(actions.is_empty(), "nothing committed while dragging");
    // Shown at once.
    assert!(paint(&mut view, &s).iter().any(|t| t == "+3"));
    let (actions, _) = run(&mut view, up(to), &s);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[0].output.transpose, 3);
    // Velocity in steps of five, down.
    let at = cell(&view, &s, 1, Col::Velocity);
    run(&mut view, down(at, 1), &s);
    let to = Point::new(at.x, at.y + 2.0 * DRAG_PX);
    run(&mut view, moved(to), &s);
    let (actions, _) = run(&mut view, up(to), &s);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[1].output.velocity, 90);
    // A click without a move changes nothing.
    let at = cell(&view, &s, 1, Col::Length);
    run(&mut view, down(at, 1), &s);
    let (actions, _) = run(&mut view, up(at), &s);
    assert!(actions.is_empty());
}

#[test]
fn the_type_switches_with_a_click_and_menus_choose_group_and_channel() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    assert_eq!(art(&s).articulations[0].kind, ArticulationKind::Direction);
    let at = cell(&view, &s, 0, Col::Kind);
    let (actions, _) = run(&mut view, down(at, 1), &s);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[0].kind, ArticulationKind::Attribute);
    let a = choose(&mut view, &s, 0, Col::Group, "Group 3");
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[0].group, 2);
    let a = choose(&mut view, &s, 1, Col::Channel, "Channel 4");
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[1].output.channel, Some(3));
    let a = choose(&mut view, &s, 1, Col::Channel, "As the Track's Notes");
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[1].output.channel, None);
    // The row's menu has them too.
    let at = cell(&view, &s, 2, Col::Name);
    let (_, requests) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: at,
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &s,
    );
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!("the row's menu");
    };
    let a = pick(items, "Type > Attribute (on a note)");
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).articulations[2].kind, ArticulationKind::Attribute);
    assert_eq!(view.selected(), Some(Row::Articulation(2)));
}

#[test]
fn rows_move_by_their_handle_and_with_alt_and_the_arrows() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    let names = |s: &Session| -> Vec<String> {
        art(s)
            .articulations
            .iter()
            .map(|a| a.name.clone())
            .collect()
    };
    let before = names(&s);
    let at = cell(&view, &s, 0, Col::Handle);
    run(&mut view, down(at, 1), &s);
    let to = cell(&view, &s, 2, Col::Handle);
    let (actions, _) = run(&mut view, moved(to), &s);
    assert!(actions.is_empty());
    // Shown where it would go.
    let doc = MapsView::doc(&s).unwrap();
    assert_eq!(view.rows(doc)[2], Row::Articulation(0));
    let (actions, _) = run(&mut view, up(to), &s);
    one_edit(&mut s, actions);
    let now = names(&s);
    assert_eq!(now[2], before[0]);
    assert_eq!(now[0], before[1]);
    assert_eq!(
        view.selected(),
        Some(Row::Articulation(2)),
        "the selection follows"
    );
    // Alt+Up moves it back up a row; plain arrows choose.
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    let (actions, _) = run(&mut view, key(Key::Up, alt), &s);
    one_edit(&mut s, actions);
    assert_eq!(names(&s)[1], before[0]);
    assert_eq!(view.selected(), Some(Row::Articulation(1)));
    let (actions, _) = run(&mut view, key(Key::Down, Modifiers::NONE), &s);
    assert!(actions.is_empty());
    assert_eq!(view.selected(), Some(Row::Articulation(2)));
    // The first row goes no higher.
    run(&mut view, key(Key::Up, Modifiers::NONE), &s);
    run(&mut view, key(Key::Up, Modifiers::NONE), &s);
    run(&mut view, key(Key::Up, Modifiers::NONE), &s);
    let (actions, _) = run(&mut view, key(Key::Up, alt), &s);
    assert!(actions.is_empty());
    // Undone in one step.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(names(&s), before);
}

#[test]
fn rows_are_added_after_the_selected_one_duplicated_and_deleted() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    let n = art(&s).articulations.len();
    // Select the second, add after it.
    let at = cell(&view, &s, 1, Col::Name);
    run(&mut view, down(at, 1), &s);
    let (actions, _) = press_button(&mut view, &s, Button::Add);
    one_edit(&mut s, actions);
    let m = art(&s);
    assert_eq!(m.articulations.len(), n + 1);
    let new = &m.articulations[2];
    assert_eq!(new.name, format!("Articulation {}", n + 1));
    assert_eq!(new.group, m.articulations[1].group);
    assert!(new.output.is_empty());
    let ids: std::collections::BTreeSet<_> = m.articulations.iter().map(|a| a.id).collect();
    assert_eq!(ids.len(), n + 1, "an id of its own");
    assert_eq!(view.selected(), Some(Row::Articulation(2)));
    // Duplicated from its menu.
    let doc = MapsView::doc(&s).unwrap();
    let items = view.row_menu(doc, Row::Articulation(0), &s);
    one_edit(&mut s, vec![pick(&items, "Duplicate")]);
    let m = art(&s);
    assert_eq!(m.articulations[1].name, "Sustain copy");
    assert_eq!(m.articulations[1].output, m.articulations[0].output);
    assert_ne!(m.articulations[1].id, m.articulations[0].id);
    // Deleted with Delete.
    paint(&mut view, &s);
    let at = cell(&view, &s, 1, Col::Name);
    run(&mut view, down(at, 1), &s);
    let (actions, _) = run(&mut view, key(Key::Delete, Modifiers::NONE), &s);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations.len(), n + 1);
    assert_eq!(art(&s).articulations[1].name, "Staccato");
}

#[test]
fn combinations_are_added_and_their_articulations_chosen_per_group() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    let (actions, _) = press_button(&mut view, &s, Button::AddCombination);
    one_edit(&mut s, actions);
    let m = art(&s);
    let sus = m.find("Sustain").unwrap().id;
    let non = m.find("Non legato").unwrap().id;
    let leg = m.find("Legato").unwrap().id;
    assert_eq!(m.combinations[0].articulations, vec![sus, non]);
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t == "Sustain + Non legato"));
    // The combination's row comes after the line over the combinations.
    let combo = m.articulations.len() + 1;
    let doc = MapsView::doc(&s).unwrap();
    assert_eq!(view.rows(doc)[combo], Row::Combination(0));
    let a = choose(&mut view, &s, combo, Col::Members, "Group 2 > Legato");
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).combinations[0].articulations, vec![sus, leg]);
    // A combination of two keeps both.
    let at = cell(&view, &s, combo, Col::Members);
    let (_, requests) = run(&mut view, down(at, 1), &s);
    run(&mut view, up(at), &s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("its menu");
    };
    let group1 = items.iter().find(|i| i.label == "Group 1").unwrap();
    assert!(
        group1
            .children
            .iter()
            .any(|i| i.label == "None (a combination needs two)" && i.action.is_none())
    );
    // What it sends, typed.
    let a = type_into(&mut view, &s, combo, Col::Keyswitches, "C2").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).combinations[0].output.keyswitches[0].key, 36);
    let a = type_into(&mut view, &s, combo, Col::Transpose, "+12").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(art(&s).combinations[0].output.transpose, 12);
    // Deleted.
    let at = cell(&view, &s, combo, Col::Keyswitches);
    run(&mut view, down(at, 1), &s);
    assert_eq!(view.selected(), Some(Row::Combination(0)));
    let (actions, _) = run(&mut view, key(Key::Backspace, Modifiers::NONE), &s);
    one_edit(&mut s, actions);
    assert!(art(&s).combinations.is_empty());
    // One group only: no combination to add.
    let mut one = art(&s);
    one.articulations.retain(|a| a.group == 0);
    s.dispatch(Action::Maps(MapOp::EditArticulationMap(Box::new(one))))
        .unwrap();
    let (actions, _) = press_button(&mut view, &s, Button::AddCombination);
    assert!(actions.is_empty());
}

#[test]
fn drum_sounds_reorder_take_typed_pitches_menus_and_learn() {
    let (mut s, t) = drums();
    let mut view = MapsView::new(Theme::default());
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t == "General MIDI ▾"));
    assert!(texts.iter().any(|t| t == "Bass Drum 1"));
    assert!(texts.iter().any(|t| t == "C2 · 36"));
    let before = drum(&s);
    // Dragged by the handle: one step.
    let at = cell(&view, &s, 0, Col::Handle);
    run(&mut view, down(at, 1), &s);
    let to = cell(&view, &s, 3, Col::Handle);
    run(&mut view, moved(to), &s);
    let (actions, _) = run(&mut view, up(to), &s);
    one_edit(&mut s, actions);
    assert_eq!(drum(&s).sounds[3], before.sounds[0]);
    s.dispatch(Action::Undo).unwrap();
    // A pitch typed onto another sound's: the two swap.
    let a = type_into(&mut view, &s, 0, Col::Pitch, "D2").unwrap();
    one_edit(&mut s, vec![a]);
    let m = drum(&s);
    assert_eq!(m.sounds[0].pitch, 38);
    assert_eq!(m.sound(36).unwrap().name, "Acoustic Snare");
    // The note it plays; its own pitch again is as written.
    let a = type_into(&mut view, &s, 1, Col::Output, "C3").unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(drum(&s).sounds[1].output_note, Some(48));
    let pitch = drum(&s).sounds[1].pitch;
    let a = type_into(&mut view, &s, 1, Col::Output, &pitch.to_string()).unwrap();
    one_edit(&mut s, vec![a]);
    assert_eq!(drum(&s).sounds[1].output_note, None);
    // Channel, grid and port from their menus.
    let a = choose(&mut view, &s, 2, Col::Channel, "Channel 10");
    one_edit(&mut s, vec![a]);
    let a = choose(&mut view, &s, 2, Col::Grid, "1/16T");
    one_edit(&mut s, vec![a]);
    let m = drum(&s);
    assert_eq!(m.sounds[2].channel, Some(9));
    assert_eq!(m.sounds[2].quantize, Some(GridDivision::Triplet(16)));
    s.add_virtual_midi_output("Drum Brain");
    let a = choose(&mut view, &s, 2, Col::Port, "Drum Brain");
    one_edit(&mut s, vec![a]);
    assert_eq!(
        drum(&s).sounds[2].port.as_deref(),
        Some("virtual:Drum Brain")
    );
    assert!(paint(&mut view, &s).iter().any(|t| t == "Drum Brain"));
    // The name.
    let a = type_into(&mut view, &s, 2, Col::Name, "Kick In").unwrap();
    one_edit(&mut s, vec![a]);
    // Learn: a click starts it, the row shows it, another stops it.
    let at = cell(&view, &s, 4, Col::Learn);
    let (actions, _) = run(&mut view, down(at, 1), &s);
    run(&mut view, up(at), &s);
    let p4 = drum(&s).sounds[4].pitch;
    assert_eq!(
        actions,
        vec![Action::Maps(MapOp::LearnDrumPitch {
            map: drum(&s).id,
            pitch: p4
        })]
    );
    apply(&mut s, actions);
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t == "Play a key…"));
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Learning: play the key for"))
    );
    let (actions, _) = run(&mut view, down(at, 1), &s);
    run(&mut view, up(at), &s);
    assert_eq!(actions, vec![Action::Maps(MapOp::CancelLearn)]);
    // Escape stops it too.
    let map = drum(&s).id;
    apply(
        &mut s,
        vec![Action::Maps(MapOp::LearnDrumPitch { map, pitch: p4 })],
    );
    let (actions, _) = run(&mut view, key(Key::Escape, Modifiers::NONE), &s);
    assert_eq!(actions, vec![Action::Maps(MapOp::CancelLearn)]);
    // Added after the selected sound on the next free key.
    let at = cell(&view, &s, 0, Col::Name);
    run(&mut view, down(at, 1), &s);
    let (actions, _) = press_button(&mut view, &s, Button::Add);
    one_edit(&mut s, actions);
    let m = drum(&s);
    assert_eq!(m.sounds.len(), before.sounds.len() + 1);
    let new = &m.sounds[1];
    assert!(m.sounds.iter().filter(|x| x.pitch == new.pitch).count() == 1);
    let _ = t;
}

#[test]
fn the_toolbar_chooses_renames_imports_and_exports() {
    let (mut s, t) = drums();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    // The chooser lists both maps; choosing one shows it.
    let (_, requests) = press_button(&mut view, &s, Button::Map);
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!("the chooser");
    };
    let strings = pick(items, "Strings (Articulations)");
    assert!(
        items
            .iter()
            .any(|i| i.label == "General MIDI (Drums)" && i.checked == Some(true))
    );
    assert!(
        matches!(pick(items, "New > Articulation Map: Brass"), Action::Maps(MapOp::NewArticulationMap { track: Some(x), .. }) if x == t)
    );
    assert!(matches!(
        pick(items, "Delete This Map"),
        Action::Maps(MapOp::RemoveDrumMap(_))
    ));
    apply(&mut s, vec![strings]);
    assert!(matches!(MapsView::doc(&s), Some(Doc::Art(_))));
    let id = art(&s).id;
    // Renamed.
    let (_, requests) = press_button(&mut view, &s, Button::Rename);
    let HostRequest::TextInput {
        initial, commit, ..
    } = requests.into_iter().next().unwrap()
    else {
        panic!("a text entry");
    };
    assert_eq!(initial, "Strings");
    one_edit(&mut s, vec![commit("Orchestra").unwrap()]);
    assert_eq!(art(&s).name, "Orchestra");
    // Imported for the track edited for.
    let (_, requests) = press_button(&mut view, &s, Button::Import);
    let HostRequest::ChooseFiles { choice, commit } = requests.into_iter().next().unwrap() else {
        panic!("a chooser");
    };
    let FileChoice::Open { filters, .. } = choice else {
        panic!("files to open");
    };
    assert!(filters[0].1.contains(&"*.expressionmap".to_string()));
    let path = std::path::PathBuf::from("/maps/Legato.expressionmap");
    assert_eq!(
        commit(vec![path.clone()]),
        Some(Action::Maps(MapOp::ImportArticulationMap {
            track: Some(t),
            path
        }))
    );
    // Exported as JSON.
    let (_, requests) = press_button(&mut view, &s, Button::Export);
    let HostRequest::ChooseFiles { choice, commit } = requests.into_iter().next().unwrap() else {
        panic!("a chooser");
    };
    let FileChoice::Save { name, .. } = choice else {
        panic!("a file to write");
    };
    assert_eq!(name, "Orchestra.json");
    assert_eq!(
        commit(vec!["/maps/orchestra".into()]),
        Some(Action::Maps(MapOp::SaveArticulationMap {
            map: id,
            path: Some("/maps/orchestra.json".into())
        }))
    );
    let (actions, _) = press_button(&mut view, &s, Button::SaveUser);
    assert_eq!(
        actions,
        vec![Action::Maps(MapOp::SaveArticulationMap {
            map: id,
            path: None
        })]
    );
}

#[test]
fn without_a_map_it_says_how_to_get_one() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = MapsView::new(Theme::default());
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t == "No Map ▾"));
    assert!(texts.iter().any(|t| t.starts_with("No map:")));
    assert_eq!(MapsView::buttons(SIZE, &s).len(), 1, "only the chooser");
    let (_, requests) = press_button(&mut view, &s, Button::Map);
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!("the chooser");
    };
    let gm = pick(items, "New > Drum Map: General MIDI");
    apply(&mut s, vec![gm]);
    assert!(matches!(MapsView::doc(&s), Some(Doc::Drum(_))));
}

#[test]
fn it_paints_both_kinds_in_every_theme() {
    for make in [session, drums] {
        let (s, _) = make();
        let mut view = MapsView::new(Theme::default());
        for theme in Theme::all() {
            let mut p = RecordingPainter::new();
            view.paint(&mut p, SIZE, &s, &theme);
            assert!(p.texts().contains(&"Map Editor"), "{}", theme.id);
        }
        // Narrow: scrolls sideways.
        let narrow = Size { w: 520.0, h: 300.0 };
        let info = view
            .scroll_info(ScrollAxis::Horizontal, narrow, &s)
            .unwrap();
        assert!(info.content > info.viewport);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, narrow, &s, &Theme::default());
    }
    let (s, _) = session();
    let mut view = MapsView::new(Theme::default());
    let texts = paint(&mut view, &s);
    assert!(texts.iter().any(|t| t.starts_with("Combinations")));
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Articulation map · used by Lead Synth"))
    );
    assert!(texts.iter().any(|t| t == "→ Direction"));
    assert!(texts.iter().any(|t| t == "• Attribute"));
}

fn node_named<'a>(nodes: &'a [AccessNode<Action>], label: &str) -> &'a AccessNode<Action> {
    faderframe_ui_canvas::access::focus_order(nodes)
        .into_iter()
        .find(|n| n.label == label)
        .unwrap_or_else(|| panic!("no {label}"))
}

fn press(
    view: &mut MapsView,
    s: &Session,
    id: u64,
    k: Key,
    modifiers: Modifiers,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let nodes = view.accessible(SIZE, s);
    press_key(view, &nodes, id, k, modifiers, SIZE, s).expect("the key does something")
}

#[test]
fn the_keyboard_reaches_every_cell() {
    let (mut s, _) = session();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    let nodes = view.accessible(SIZE, &s);
    // The toolbar's buttons, the rows with their cells.
    let map = node_named(&nodes, "Map: Strings, articulation map");
    assert!(map.menu && map.fixed_y);
    let (_, requests) = press(&mut view, &s, map.id, Key::Enter, Modifiers::NONE);
    assert!(matches!(requests[0], HostRequest::ContextMenu { .. }));
    let row = node_named(&nodes, "Sustain (sus.), group 1, direction");
    assert_eq!(row.description, "sends C1");
    assert!(row.menu);
    let (_, requests) = press(&mut view, &s, row.id, Key::Menu, Modifiers::NONE);
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!("the row's menu");
    };
    assert!(items.iter().any(|i| i.label == "Delete Articulation"));
    assert_eq!(view.selected(), Some(Row::Articulation(0)));
    // A name: Enter types it.
    let name = find_node(&nodes, row.id)
        .unwrap()
        .children
        .iter()
        .find(|n| n.label == "Name: Sustain")
        .unwrap()
        .id;
    let (_, requests) = press(&mut view, &s, name, Key::Enter, Modifiers::NONE);
    let HostRequest::TextInput { commit, .. } = requests.into_iter().next().unwrap() else {
        panic!("a text entry");
    };
    one_edit(&mut s, vec![commit("Sustained").unwrap()]);
    // Numbers step with the arrows.
    let nodes = view.accessible(SIZE, &s);
    let row = node_named(&nodes, "Sustained (sus.), group 1, direction");
    let cells = &find_node(&nodes, row.id).unwrap().children;
    let transpose = cells.iter().find(|n| n.label == "Transpose").unwrap();
    assert_eq!(transpose.value.as_ref().unwrap().text, "0 semitones");
    let id = transpose.id;
    let (actions, _) = press(&mut view, &s, id, Key::Up, Modifiers::NONE);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[0].output.transpose, 1);
    // A channel steps up from the track's.
    let channel = cells
        .iter()
        .find(|n| n.label == "Channel: the track's")
        .unwrap()
        .id;
    let (actions, _) = press(&mut view, &s, channel, Key::Up, Modifiers::NONE);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[0].output.channel, Some(0));
    let (actions, _) = press(&mut view, &s, channel, Key::Down, Modifiers::NONE);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[0].output.channel, None);
    // The type switches with Enter.
    let kind = cells
        .iter()
        .find(|n| n.label == "Type: Direction")
        .unwrap()
        .id;
    let (actions, _) = press(&mut view, &s, kind, Key::Enter, Modifiers::NONE);
    one_edit(&mut s, actions);
    assert_eq!(art(&s).articulations[0].kind, ArticulationKind::Attribute);
    // Ids stay with the articulation as it moves.
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    let (actions, _) = run(&mut view, key(Key::Down, alt), &s);
    one_edit(&mut s, actions);
    let nodes = view.accessible(SIZE, &s);
    assert!(find_node(&nodes, row.id).is_some());
    assert!(find_node(&nodes, kind).is_some());
    // Drum sounds: Learn switches with Enter.
    let (mut s, _) = drums();
    let mut view = MapsView::new(Theme::default());
    paint(&mut view, &s);
    let nodes = view.accessible(SIZE, &s);
    let kick = node_named(&nodes, "Bass Drum 1, on C2 (36)");
    let learn = kick
        .children
        .iter()
        .find(|n| n.label == "Learn the pitch from MIDI input")
        .unwrap();
    assert_eq!(learn.checked, Some(false));
    let (actions, _) = press(&mut view, &s, learn.id, Key::Enter, Modifiers::NONE);
    assert!(matches!(
        actions[..],
        [Action::Maps(MapOp::LearnDrumPitch { pitch: 36, .. })]
    ));
    apply(&mut s, actions);
    let nodes = view.accessible(SIZE, &s);
    assert!(
        node_named(&nodes, "Bass Drum 1, on C2 (36), learning its pitch")
            .children
            .iter()
            .any(|n| n.checked == Some(true))
    );
    // The pitch steps (swapping with the sound above it).
    let pitch = node_named(&nodes, "Pitch");
    let (actions, _) = press(&mut view, &s, pitch.id, Key::Up, Modifiers::NONE);
    one_edit(&mut s, actions);
    assert_eq!(drum(&s).sounds[0].pitch, 37);
}

#[test]
fn the_map_shown_is_said_when_it_changes() {
    let (mut s, t) = session();
    let mut view = MapsView::new(Theme::default());
    assert!(view.access_news(&s).is_empty(), "the first call only looks");
    assert!(view.access_news(&s).is_empty());
    s.dispatch(Action::Maps(MapOp::NewDrumMap {
        track: Some(t),
        from: DrumMapSource::Gm,
    }))
    .unwrap();
    assert_eq!(
        view.access_news(&s),
        vec!["Editing the drum map General MIDI".to_string()]
    );
}
