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

fn menu(req: Vec<HostRequest<Action>>) -> Vec<MenuItem<Action>> {
    match req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    {
        Some(HostRequest::ContextMenu { items, .. }) => items,
        _ => panic!("a menu"),
    }
}

fn pick(items: &[MenuItem<Action>], label: &str) -> Action {
    items
        .iter()
        .find(|i| i.label.contains(label))
        .and_then(|i| i.action.clone())
        .unwrap_or_else(|| panic!("{label}"))
}

fn type_into(view: &mut AlbumView, s: &mut Session, at: Point, text: &str) {
    let req = run(view, s, down(at, 2));
    let Some(HostRequest::TextInput { commit, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("text input")
    };
    s.dispatch(commit(text).unwrap()).unwrap();
}

#[test]
fn codes_crossfades_inserts_and_the_cd_master() {
    let mut s = session();
    let mut view = AlbumView::new(Theme::default());
    let l = view.layout(SIZE, &s);
    // A typed ISRC is normalised; "×2" crossfades, a number pauses.
    let isrc = view.cell(&l, 0, Column::Isrc).center();
    type_into(&mut view, &mut s, isrc, "gb-xyz-26-00042");
    assert_eq!(s.project().album.songs[0].isrc, "GBXYZ2600042");
    let pause = view.cell(&l, 1, Column::Pause).center();
    type_into(&mut view, &mut s, pause, "×2");
    assert_eq!(s.project().album.songs[1].crossfade, 2.0);
    type_into(&mut view, &mut s, pause, "1.5");
    let song = &s.project().album.songs[1];
    assert_eq!((song.pause, song.crossfade), (1.5, 0.0));
    // The inserts cell: add one (the plugin browser for the song), then
    // hear them on the master.
    let id = s.project().album.songs[0].id;
    let cell = view.cell(&l, 0, Column::Inserts).center();
    let items = menu(run(&mut view, &mut s, down(cell, 1)));
    assert!(matches!(
        pick(&items, "Add Insert"),
        Action::OpenPluginBrowser {
            target: PluginTarget::Song(x),
            ..
        } if x == id
    ));
    let master = s.project().master_id().unwrap();
    s.place_plugin(
        master,
        PluginTarget::Song(id),
        faderframe_project::PluginRef::builtin(faderframe_core::builtin::GAIN, "Gain"),
    )
    .unwrap();
    assert_eq!(s.project().album.songs[0].inserts.len(), 1);
    let items = menu(run(&mut view, &mut s, down(cell, 1)));
    s.dispatch(pick(&items, "Hear Them")).unwrap();
    assert_eq!(s.album_monitor(), Some(id));
    s.dispatch(pick(&items, "Bypass")).unwrap();
    assert!(s.project().album.songs[0].inserts[0].bypass);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    // The CD master menu and the release details.
    let items = menu(press(&mut view, &mut s, Button::Cd));
    s.dispatch(pick(&items, "Write a CD Master")).unwrap();
    assert!(s.project().album.settings.ddp);
    press(&mut view, &mut s, Button::Release);
    assert!(
        s.take_ui_requests()
            .contains(&faderframe_session::UiRequest::AlbumDetails(None))
    );
}
