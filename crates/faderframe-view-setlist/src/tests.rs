#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SIZE: Size = Size::new(1200.0, 700.0);

fn session() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Setlist(SetlistOp::FromSections))
        .unwrap();
    s
}

fn run(
    view: &mut SetlistView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn press(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

#[test]
fn the_list_shows_the_songs_and_edits_them() {
    let mut s = session();
    let mut view = SetlistView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(
        texts.contains(&"Intro") && texts.contains(&"Verse"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.starts_with("2 songs")));
    // Renaming by a double-click on the name.
    let name = SetlistView::cell(1, view.row_rect(0, SIZE)).center();
    let (_, requests) = run(&mut view, press(name, 2), &s);
    let Some(HostRequest::TextInput { commit, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("a text entry");
    };
    s.dispatch(commit("Opener").unwrap()).unwrap();
    assert_eq!(s.setlist().songs[0].name, "Opener");
    // What follows: from the Then column's menu.
    let then = SetlistView::cell(4, view.row_rect(0, SIZE)).center();
    let (_, requests) = run(&mut view, press(then, 1), &s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("the menu");
    };
    let on = items.iter().find(|i| i.label == "Play on").unwrap();
    s.dispatch(on.action.clone().unwrap()).unwrap();
    assert_eq!(s.setlist().songs[0].then, AfterSong::Continue);
    // Alt+Down moves the selected song.
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Down,
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
        },
        &s,
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.setlist().songs[1].name, "Opener");
}

#[test]
fn the_stage_screen_plays_the_show() {
    let mut s = session();
    let mut view = SetlistView::new(Theme::default());
    // Show Mode from the header.
    let (_, show, _) = SetlistView::buttons(SIZE)[2];
    let (actions, _) = run(&mut view, press(show.center(), 1), &s);
    assert_eq!(actions, [Action::Show(ShowOp::Enter)]);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(s.show_mode());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(texts.contains(&"SHOW"), "{texts:?}");
    assert!(texts.contains(&"Intro"));
    assert!(texts.contains(&"NEXT"), "the next song marked");
    assert!(texts.iter().any(|t| t.starts_with("Song 1 of 2")));
    // The keys: → the next song, Space plays, Esc leaves.
    for (key, op) in [
        (Key::Right, ShowOp::Next),
        (Key::Space, ShowOp::PlayStop),
        (Key::Escape, ShowOp::Leave),
    ] {
        let (actions, _) = run(
            &mut view,
            ViewEvent::Key {
                key,
                modifiers: Modifiers::NONE,
            },
            &s,
        );
        assert_eq!(actions, [Action::Show(op)]);
    }
    // The buttons.
    let l = SetlistView::stage(SIZE);
    assert_eq!(view.hit(l.next.center(), SIZE, &s), Some(Hit::Next));
    assert_eq!(view.hit(l.play.center(), SIZE, &s), Some(Hit::PlayStop));
    let row = SetlistView::stage_row(&l, 1).center();
    let (actions, _) = run(&mut view, press(row, 2), &s);
    assert_eq!(actions, [Action::Show(ShowOp::Go(1))]);
}
