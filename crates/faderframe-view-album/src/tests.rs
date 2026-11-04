use super::*;
use faderframe_engine::EngineConfig;
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

/// The demo without its sections (each test makes the ones it needs).
fn demo() -> Session {
    let config = EngineConfig::default();
    let mut p = faderframe_project::demo::demo_project(config.sample_rate);
    p.sections.clear();
    Session::new(p, None, config).unwrap()
}

const SIZE: Size = Size::new(1400.0, 320.0);

fn session() -> Session {
    let mut s = demo();
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

#[test]
fn the_vinyl_menu_sides_and_side_breaks() {
    let mut s = session();
    let mut view = AlbumView::new(Theme::default());
    press(&mut view, &mut s, Button::AddProject);
    // Off: the songs are numbered.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"1") && !p.texts().contains(&"A1"));
    // The menu turns the premaster on with a format.
    let items = menu(press(&mut view, &mut s, Button::Vinyl));
    s.dispatch(pick(&items, "7″ single at 45 rpm")).unwrap();
    let v = &s.project().album.settings.vinyl;
    assert!(v.enabled && v.format == VinylFormat::Single7At45);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(texts.contains(&"A1"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t.starts_with("Vinyl 7″ 45: A ")),
        "{texts:?}"
    );
    assert!(texts.contains(&"Vinyl · 7″ 45 ▾"), "{texts:?}");
    // A side break from a row's menu: song 2 starts side B.
    let l = view.layout(SIZE, &s);
    let at = view.cell(&l, 1, Column::Title).center();
    let req = run(
        &mut view,
        &mut s,
        ViewEvent::PointerDown {
            pos: at,
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
    );
    let items = menu(req);
    s.dispatch(pick(&items, "Start a New Side Here")).unwrap();
    let sides = s.vinyl_sides().unwrap();
    assert_eq!(sides[1].songs.start, 1, "{sides:?}");
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"B1"));
    // Peaks and limiting from the menu.
    let items = menu(press(&mut view, &mut s, Button::Vinyl));
    s.dispatch(pick(&items, "Peaks at −1 dBTP")).unwrap();
    assert_eq!(s.project().album.settings.vinyl.peak, -1.0);
}

#[test]
fn the_qc_menu_chooses_the_exports_spec_and_shows_the_report() {
    let mut s = session();
    let mut view = AlbumView::new(Theme::default());
    assert_eq!(
        AlbumView::button_label(Button::Qc, &s),
        "QC ▾",
        "no report by default"
    );
    let items = menu(press(&mut view, &mut s, Button::Qc));
    // No report, every spec, and the last export's report (none yet).
    assert_eq!(items.len(), faderframe_session::qc::SPECS.len() + 2);
    assert_eq!(items[0].checked, Some(true));
    let report = items.last().unwrap();
    assert!(report.action.is_none(), "nothing exported yet");
    s.dispatch(pick(&items, "EBU R128 (broadcast")).unwrap();
    assert_eq!(
        s.project().album.settings.qc_spec.as_deref(),
        Some("ebu-r128")
    );
    assert_eq!(AlbumView::button_label(Button::Qc, &s), "QC · EBU R128 ▾");
    // Undone with the album's other settings.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().album.settings.qc_spec, None);
}

mod keyboard {
    use super::*;
    use faderframe_ui_canvas::access::{find_node, focus_order, press_key};
    use faderframe_ui_canvas::{AccessNode, AccessRole};

    fn nodes(view: &AlbumView, s: &Session) -> Vec<AccessNode<Action>> {
        view.accessible(SIZE, s)
    }

    fn named(view: &AlbumView, s: &Session, label: &str) -> AccessNode<Action> {
        let nodes = nodes(view, s);
        let found = focus_order(&nodes)
            .into_iter()
            .chain(nodes.iter())
            .find(|n| n.label.starts_with(label))
            .cloned();
        found.unwrap_or_else(|| panic!("no control '{label}'"))
    }

    /// The keyboard presses `key` on the control named `label`: actions
    /// to the session, requests back.
    fn key(view: &mut AlbumView, s: &mut Session, label: &str, k: Key) -> Vec<HostRequest<Action>> {
        let id = named(view, s, label).id;
        let nodes = nodes(view, s);
        let (actions, requests) = press_key(view, &nodes, id, k, Modifiers::NONE, SIZE, s)
            .unwrap_or_else(|| panic!("{k:?} does something on '{label}'"));
        for a in actions {
            s.dispatch(a).unwrap();
        }
        requests
    }

    /// The toolbar, the player, the songs and the export folder: every
    /// button and every song's changing part is a control the keyboard
    /// reaches; menu buttons open their menus, "+ Files…" its chooser.
    #[test]
    fn the_album_reads_as_a_toolbar_a_player_and_its_songs() {
        let mut s = session();
        let mut view = AlbumView::new(Theme::default());
        assert_eq!(view.accessible_name().as_deref(), Some("Album"));
        let tree = nodes(&view, &s);
        let roles: Vec<AccessRole> = tree.iter().map(|n| n.role).collect();
        assert_eq!(
            roles,
            [
                AccessRole::Toolbar,
                AccessRole::Group,
                AccessRole::List,
                AccessRole::Label,
                AccessRole::Button
            ]
        );
        // Every toolbar button, in the order shown.
        let l = view.layout(SIZE, &s);
        let order = focus_order(&tree);
        assert!(order.len() > l.buttons.len());
        for (b, r) in &l.buttons {
            let n = order
                .iter()
                .find(|n| n.bounds == *r)
                .unwrap_or_else(|| panic!("{b:?}"));
            assert!(!n.label.is_empty());
            assert!(
                n.fixed_x && n.fixed_y,
                "{b:?} stays put when the list scrolls"
            );
        }
        // Menu buttons: Enter opens the menu.
        let target = named(&view, &s, "Loudness target");
        assert!(target.menu);
        let req = key(&mut view, &mut s, "Loudness target", Key::Enter);
        assert_eq!(menu(req).len(), LOUDNESS_TARGETS.len() + 1);
        // "+ This project" adds a song; the album file is a toggle.
        key(&mut view, &mut s, "Add this project", Key::Enter);
        assert_eq!(s.project().album.songs.len(), 3);
        let album_file = named(&view, &s, "Album file and cue sheet");
        assert_eq!(album_file.role, AccessRole::ToggleButton);
        let on = s.project().album.settings.album_file;
        assert_eq!(album_file.checked, Some(on));
        key(&mut view, &mut s, "Album file and cue sheet", Key::Enter);
        assert_eq!(named(&view, &s, "Album file").checked, Some(!on));
        // "+ Files…" and the export folder: their choosers.
        let req = key(&mut view, &mut s, "Add files", Key::Enter);
        assert!(matches!(
            req.first(),
            Some(HostRequest::ChooseFiles {
                choice: FileChoice::Open { .. },
                ..
            })
        ));
        let req = key(&mut view, &mut s, "Export to ", Key::Enter);
        assert!(matches!(
            req.first(),
            Some(HostRequest::ChooseFiles {
                choice: FileChoice::Folder { .. },
                ..
            })
        ));
        // The player: nothing to skip in until the album plays.
        assert!(named(&view, &s, "Previous song").disabled);
        assert!(named(&view, &s, "Back to the project").disabled);
        assert!(!named(&view, &s, "Play album").disabled);
        // The songs: read with their loudness, not analysed yet.
        let tree = nodes(&view, &s);
        let list = tree.iter().find(|n| n.role == AccessRole::List).unwrap();
        assert_eq!(list.children.len(), 3);
        assert!(
            list.children[0].label.starts_with("1. ")
                && list.children[0].label.ends_with("not analysed"),
            "{}",
            list.children[0].label
        );
        // The first song has no pause before it; the others do.
        let parts = |i: usize| -> Vec<String> {
            list.children[i]
                .children
                .iter()
                .map(|n| n.label.clone())
                .collect()
        };
        assert!(!parts(0).iter().any(|p| p == "Pause before"));
        assert!(
            parts(1).iter().any(|p| p == "Pause before"),
            "{:?}",
            parts(1)
        );
    }

    /// Enter selects a song (the arrows move the selection, Delete removes
    /// it), its menu is the row's; its trim and pause step with the arrows,
    /// its title, fades and ISRC are typed, its inserts' menu opens.
    #[test]
    fn songs_are_selected_stepped_typed_and_have_their_menus() {
        let mut s = session();
        let mut view = AlbumView::new(Theme::default());
        let first = s.project().album.songs[0].id;
        let second = s.project().album.songs[1].id;
        let song = |view: &AlbumView, s: &Session, i: usize| {
            nodes(view, s)
                .iter()
                .find(|n| n.role == AccessRole::List)
                .unwrap()
                .children[i]
                .clone()
        };
        let row = song(&view, &s, 1);
        assert_eq!(row.selected, Some(false));
        let tree = nodes(&view, &s);
        press_key(
            &mut view,
            &tree,
            row.id,
            Key::Enter,
            Modifiers::NONE,
            SIZE,
            &s,
        )
        .unwrap();
        assert_eq!(view.selected, Some(second));
        assert_eq!(song(&view, &s, 1).selected, Some(true));
        // The arrows are the list's own (the keyboard follows the
        // selection).
        let tree = nodes(&view, &s);
        assert!(press_key(&mut view, &tree, row.id, Key::Up, Modifiers::NONE, SIZE, &s).is_none());
        run(
            &mut view,
            &mut s,
            ViewEvent::Key {
                key: Key::Up,
                modifiers: Modifiers::NONE,
            },
        );
        assert_eq!(view.selected, Some(first));
        // The row's menu (Shift+F10, the Menu key).
        let tree = nodes(&view, &s);
        let (_, req) = press_key(
            &mut view,
            &tree,
            row.id,
            Key::F(10),
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            SIZE,
            &s,
        )
        .unwrap();
        let items = menu(req);
        assert!(items.iter().any(|i| i.label == "Play from Here"));
        s.dispatch(pick(&items, "Fade Out 5 s")).unwrap();
        assert_eq!(s.project().album.songs[1].fade_out, 5.0);
        // Trim: a tenth of a dB a step.
        let trim = song(&view, &s, 0)
            .children
            .into_iter()
            .find(|n| n.label == "Trim")
            .unwrap();
        assert_eq!(trim.role, AccessRole::SpinButton);
        let tree = nodes(&view, &s);
        let (actions, _) = press_key(
            &mut view,
            &tree,
            trim.id,
            Key::Up,
            Modifiers::NONE,
            SIZE,
            &s,
        )
        .unwrap();
        for a in actions {
            s.dispatch(a).unwrap();
        }
        assert!((s.project().album.songs[0].gain_db - 0.1).abs() < 1e-6);
        assert_eq!(
            find_node(&nodes(&view, &s), trim.id)
                .and_then(|n| n.value.as_ref())
                .map(|v| v.text.clone()),
            Some("+0.1 dB".to_string())
        );
        // The pause: a step, then typed into a crossfade.
        let before = s.project().album.songs[1].pause;
        key(&mut view, &mut s, "Pause before", Key::Up);
        let paused = s.project().album.songs[1].pause;
        assert!((paused - before - 0.1).abs() < 1e-4, "{before} → {paused}");
        let req = key(&mut view, &mut s, "Pause before", Key::Enter);
        let Some(HostRequest::TextInput { commit, .. }) = req.into_iter().next() else {
            panic!("a text entry")
        };
        s.dispatch(commit("×2").unwrap()).unwrap();
        assert_eq!(s.project().album.songs[1].crossfade, 2.0);
        assert!(
            named(&view, &s, "Crossfade from the previous song")
                .value
                .is_some()
        );
        // The title, typed.
        let req = key(&mut view, &mut s, "Title: ", Key::Enter);
        let Some(HostRequest::TextInput { commit, .. }) = req.into_iter().next() else {
            panic!("a text entry")
        };
        s.dispatch(commit("Overture").unwrap()).unwrap();
        assert_eq!(s.project().album.songs[0].title, "Overture");
        assert!(song(&view, &s, 0).label.starts_with("1. Overture"));
        // The inserts' menu.
        let req = key(&mut view, &mut s, "Inserts: none", Key::Enter);
        assert!(menu(req).iter().any(|i| i.label == "Add Insert…"));
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

    /// The list scrolls to the song the keyboard reaches.
    #[test]
    fn the_list_scrolls_to_the_song_selected() {
        let mut s = session();
        for _ in 0..12 {
            s.dispatch(Action::Album(AlbumAction::AddThisProject))
                .unwrap();
        }
        let mut view = AlbumView::new(Theme::default());
        let info = view
            .scroll_info(faderframe_ui_canvas::ScrollAxis::Vertical, SIZE, &s)
            .unwrap();
        assert!(info.content > info.viewport);
        let last = nodes(&view, &s)
            .iter()
            .find(|n| n.role == AccessRole::List)
            .unwrap()
            .children
            .last()
            .unwrap()
            .clone();
        assert!(
            last.bounds.bottom() > info.start + info.viewport,
            "off the view"
        );
        let tree = nodes(&view, &s);
        press_key(
            &mut view,
            &tree,
            last.id,
            Key::Enter,
            Modifiers::NONE,
            SIZE,
            &s,
        )
        .unwrap();
        let l = view.layout(SIZE, &s);
        let i = s.project().album.songs.len() - 1;
        let r = view.row_rect(&l, i);
        assert!(
            r.y >= l.list.y && r.bottom() <= l.list.bottom(),
            "{r:?} in {:?}",
            l.list
        );
    }

    /// An analysis starting and finishing is said (with the album's
    /// loudness), and nothing on the first look.
    #[test]
    fn an_analysis_finishing_is_said() {
        let mut s = session();
        let mut view = AlbumView::new(Theme::default());
        assert!(view.access_news(&s).is_empty());
        s.dispatch(Action::Album(AlbumAction::Analyse)).unwrap();
        assert_eq!(view.access_news(&s), ["Analysing the album"]);
        s.wait_album();
        let said = view.access_news(&s);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with("Analysis finished: album ") && said[0].contains("LUFS"),
            "{said:?}"
        );
        assert!(view.access_news(&s).is_empty());
        // The songs now read with their loudness.
        let tree = nodes(&view, &s);
        let list = tree.iter().find(|n| n.role == AccessRole::List).unwrap();
        assert!(
            list.children[0].label.contains("LUFS"),
            "{}",
            list.children[0].label
        );
    }
}
