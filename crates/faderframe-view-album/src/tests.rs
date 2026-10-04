use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SIZE: Size = Size::new(1400.0, 320.0);

fn session() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    for (a, b) in [(0.0, 16.0), (16.0, 32.0)] {
        s.dispatch(Action::AddSection {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        })
        .unwrap();
    }
    s.dispatch(Action::Album(AlbumAction::AddSections)).unwrap();
    s
}

/// Feed an event; apply what the view emits to the session.
fn run(view: &mut AlbumView, s: &mut Session, ev: ViewEvent) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    requests
}

fn down(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

fn drag_to(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn up(pos: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

fn button(view: &AlbumView, s: &Session, b: Button) -> Point {
    view.layout(SIZE, s)
        .buttons
        .iter()
        .find(|(x, _)| *x == b)
        .unwrap()
        .1
        .center()
}

fn press(view: &mut AlbumView, s: &mut Session, b: Button) -> Vec<HostRequest<Action>> {
    let at = button(view, s, b);
    run(view, s, down(at, 1))
}

#[test]
fn rows_paint_and_buttons_add_songs() {
    let mut s = session();
    let mut view = AlbumView::new(Theme::default());
    assert_eq!(s.project().album.songs.len(), 2);
    press(&mut view, &mut s, Button::AddProject);
    assert_eq!(s.project().album.songs.len(), 3);
    assert_eq!(s.project().album.songs[2].source, SongSource::ThisProject);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    // "+ Files…" asks for a file chooser.
    let req = press(&mut view, &mut s, Button::AddFiles);
    assert!(
        req.iter()
            .any(|r| matches!(r, HostRequest::ChooseFiles { .. }))
    );
    // The album-file toggle.
    let on = s.project().album.settings.album_file;
    press(&mut view, &mut s, Button::AlbumFile);
    assert_eq!(s.project().album.settings.album_file, !on);
    // The target menu offers the Tools view's targets.
    let req = press(&mut view, &mut s, Button::Target);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu")
    };
    assert_eq!(items.len(), LOUDNESS_TARGETS.len() + 1);
}

#[test]
fn rows_reorder_rename_and_drag_values() {
    let mut s = session();
    let mut view = AlbumView::new(Theme::default());
    let l = view.layout(SIZE, &s);
    let first = s.project().album.songs[0].id;
    // Drag the first row by its number below the second.
    let from = view.cell(&l, 0, Column::Number).center();
    run(&mut view, &mut s, down(from, 1));
    let to = Point::new(from.x, l.list.y + 2.0 * ROW_H);
    run(&mut view, &mut s, drag_to(to));
    run(&mut view, &mut s, up(to));
    assert_eq!(s.project().album.songs[1].id, first);
    // Double-click the title: a text entry that renames.
    let title = view.cell(&l, 0, Column::Title).center();
    let req = run(&mut view, &mut s, down(title, 2));
    let Some(HostRequest::TextInput { commit, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("text input")
    };
    s.dispatch(commit("Overture").unwrap()).unwrap();
    assert_eq!(s.project().album.songs[0].title, "Overture");
    // Drag the trim up 30 px: +3 dB, one undo step.
    let gain = view.cell(&l, 0, Column::Gain).center();
    run(&mut view, &mut s, down(gain, 1));
    run(
        &mut view,
        &mut s,
        drag_to(Point::new(gain.x, gain.y - 10.0)),
    );
    run(
        &mut view,
        &mut s,
        drag_to(Point::new(gain.x, gain.y - 30.0)),
    );
    run(&mut view, &mut s, up(Point::new(gain.x, gain.y - 30.0)));
    assert_eq!(s.project().album.songs[0].gain_db, 3.0);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().album.songs[0].gain_db, 0.0);
    // Typed fades.
    let fades = view.cell(&l, 0, Column::Fades).center();
    let req = run(&mut view, &mut s, down(fades, 2));
    let Some(HostRequest::TextInput { commit, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("text input")
    };
    s.dispatch(commit("0,5 / 3").unwrap()).unwrap();
    let song = &s.project().album.songs[0];
    assert_eq!((song.fade_in, song.fade_out), (0.5, 3.0));
    // Delete removes the selected song.
    run(
        &mut view,
        &mut s,
        ViewEvent::Key {
            key: Key::Delete,
            modifiers: Modifiers::NONE,
        },
    );
    assert_eq!(s.project().album.songs.len(), 1);
}

#[test]
fn numbers_accept_typed_variants() {
    assert_eq!(number("−3.5 dB"), Some(-3.5));
    assert_eq!(number("2,5"), Some(2.5));
    assert_eq!(number("+1"), Some(1.0));
    assert_eq!(number("x"), None);
    assert_eq!(duration(185.4), "3:05");
    assert_eq!(db(-0.01), "0.0 dB");
    assert_eq!(db(-1.26), "−1.3 dB");
}
