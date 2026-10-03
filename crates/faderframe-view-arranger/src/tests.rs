use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::RecordingPainter;

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn run(
    view: &mut ArrangerView,
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

fn down(pos: Point) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    }
}

fn mv(pos: Point) -> ViewEvent {
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

#[test]
fn rows_and_clips_outside_the_viewport_are_culled() {
    let mut s = session();
    for _ in 0..200 {
        s.add_track(TrackKind::Audio).unwrap();
    }
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1200.0, 500.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let rows = view.visible_rows(ArrangerView::lane_tracks(&s).len(), size);
    assert!(rows.len() <= 7, "{rows:?}");
    let texts = p.texts();
    assert!(!texts.contains(&"Audio 150"), "off-screen header painted");
    assert!(p.balanced_clips());
}

#[test]
fn zoom_keeps_the_time_under_the_pointer() {
    let s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1200.0, 600.0);
    let x = 700.0;
    let before = view.time_at(x);
    view.zoom_at(x, 2.0, &s, size);
    let after = view.time_at(x);
    assert!(
        (before.quarters() - after.quarters()).abs() < 0.05,
        "{before:?} vs {after:?}"
    );
}

#[test]
fn ruler_click_locates_with_snap() {
    let s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1200.0, 600.0);
    let x = view.x_of(MusicalTime::from_quarters(5.2));
    let (a, _) = run(&mut view, down(Point::new(x, 20.0)), size, &s);
    assert!(a.contains(&Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters_i(5)
    ))));
}

#[test]
fn dragging_a_clip_moves_it_with_snap_in_one_gesture() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let tracks = ArrangerView::lane_tracks(&s);
    let bass_row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let bass = tracks[bass_row].id;
    let clip = s.project().track(bass).unwrap().clips[0];
    let start = s.project().clip(clip).unwrap().start;
    let row = view.row_rect(bass_row, size);
    let grab = Point::new(view.x_of(start) + 30.0, row.center().y);

    let mut actions = run(&mut view, down(grab), size, &s).0;
    assert!(actions.contains(&Action::SelectClips {
        clips: vec![clip],
        mode: SelectMode::Replace
    }));
    let target = Point::new(grab.x + 4.0 * view.ppq + 3.0, grab.y); // ~4 quarters later
    actions.extend(run(&mut view, mv(Point::new(grab.x + 10.0, grab.y)), size, &s).0);
    actions.extend(run(&mut view, mv(target), size, &s).0);
    actions.extend(run(&mut view, up(target), size, &s).0);
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, Action::BeginGesture(_)))
            .count(),
        1
    );
    assert_eq!(actions.last(), Some(&Action::EndGesture));
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let moved = s.project().clip(clip).unwrap().start;
    assert_eq!(
        moved,
        start + MusicalTime::from_quarters_i(4),
        "snapped to the beat grid"
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clip(clip).unwrap().start, start);
}

#[test]
fn double_click_on_instrument_lane_creates_a_midi_clip() {
    let s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let tracks = ArrangerView::lane_tracks(&s);
    let row_i = tracks
        .iter()
        .position(|t| t.kind == TrackKind::Instrument)
        .unwrap();
    let row = view.row_rect(row_i, size);
    let x = view.x_of(MusicalTime::from_quarters(1.5)); // bar 1, before the melody clip
    let ev = ViewEvent::PointerDown {
        pos: Point::new(x, row.center().y),
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks: 2,
    };
    let (a, _) = run(&mut view, ev, size, &s);
    assert!(
        matches!(a.first(), Some(Action::CreateMidiClip { start, .. }) if *start == MusicalTime::ZERO)
    );
}

#[test]
fn header_buttons_emit_commands() {
    let s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let t = ArrangerView::lane_tracks(&s)[0];
    let l = view.header_layout(&s, t.id, size).unwrap();
    let (a, _) = run(&mut view, down(l.solo.center()), size, &s);
    assert_eq!(
        a,
        vec![Action::Edit(Command::SetTrackSolo {
            track: t.id,
            on: true
        })]
    );
}
