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

/// The demo session with its "Bass" clip turned into a three-take folder
/// (the takes reuse the clip's source).
fn session_with_takes() -> (Session, ClipId, usize) {
    let mut s = session();
    let tracks = ArrangerView::lane_tracks(&s);
    let row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let bass = tracks[row].id;
    let clip = s.project().track(bass).unwrap().clips[0];
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    let mut f = TakeFolder::new(a.length);
    for i in 0..3 {
        f.add_take(faderframe_project::Take {
            name: format!("Take {}", i + 1),
            source: a.source,
            source_offset: a.source_offset,
            start: 0,
            end: a.length,
            gain_db: 0.0,
        });
    }
    f.use_take(2);
    s.edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    (s, clip, row)
}

#[test]
fn take_lanes_open_and_comp_by_click_and_swipe() {
    let (mut s, clip, row_i) = session_with_takes();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let theme = Theme::default();
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let start = s.project().clip(clip).unwrap().start;
    let row = view.row_rect(row_i, size);
    assert_eq!(row.h, view.row_h(), "closed folder: normal row");

    // The disclosure triangle opens the take lanes; the row grows.
    let tri = Point::new(view.x_of(start) + 8.0, row.y + 8.0);
    let (a, _) = run(&mut view, down(tri), size, &s);
    assert_eq!(a, vec![Action::ToggleTakeLanes(clip)]);
    s.dispatch(a[0].clone()).unwrap();
    assert!(s.takes_open(clip));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let row = view.row_rect(row_i, size);
    assert_eq!(row.h, view.row_h() + 3.0 * LANE_H);
    assert!(p.texts().iter().any(|t| t.contains("Take 1")));
    assert!(p.balanced_clips());

    // Clicking lane 1 uses take 1 everywhere.
    let base_h = view.row_h();
    let lane_y = |k: usize| row.y + base_h + LANE_H * (k as f32 + 0.5);
    let x0 = view.x_of(start + MusicalTime::from_quarters_i(1));
    let mut actions = run(&mut view, down(Point::new(x0, lane_y(0))), size, &s).0;
    actions.extend(run(&mut view, up(Point::new(x0, lane_y(0))), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    assert_eq!(f.pieces().len(), 1);
    assert_eq!(f.pieces()[0].take, 0);

    // Swiping across lane 3 comps a section in one undo step.
    let x1 = view.x_of(start + MusicalTime::from_quarters_i(3));
    let mut actions = run(&mut view, down(Point::new(x0, lane_y(2))), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(x0 + 20.0, lane_y(2))), size, &s).0);
    actions.extend(run(&mut view, mv(Point::new(x1, lane_y(2))), size, &s).0);
    actions.extend(run(&mut view, up(Point::new(x1, lane_y(2))), size, &s).0);
    assert_eq!(
        actions.first(),
        Some(&Action::BeginGesture("Comp Takes".into()))
    );
    assert_eq!(actions.last(), Some(&Action::EndGesture));
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    let takes: Vec<usize> = f.pieces().iter().map(|p| p.take).collect();
    assert_eq!(takes, vec![0, 2, 0], "take 3 comped into the middle");
    let rate = s.project().sample_rate as f64;
    let tl = &s.project().timeline;
    let expect_a =
        tl.to_samples(start + MusicalTime::from_quarters_i(1), rate) - tl.to_samples(start, rate);
    assert_eq!(f.pieces()[1].start, expect_a, "snapped to the beat");
    s.dispatch(Action::Undo).unwrap();
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    assert_eq!(
        f.pieces().len(),
        1,
        "undo restores the comp before the swipe"
    );

    // Right-click on a lane offers take commands.
    let ev = ViewEvent::PointerDown {
        pos: Point::new(x0, lane_y(1)),
        button: PointerButton::Secondary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    };
    let (_, req) = run(&mut view, ev, size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("no menu");
    };
    let labels: Vec<_> = items.iter().map(|i| i.label.clone()).collect();
    assert!(labels.contains(&"Flatten Comp".to_string()), "{labels:?}");
    assert!(labels.iter().any(|l| l.starts_with("Use")));
}

#[test]
fn tracks_resize_by_dragging_the_header_edge_and_alt_wheel() {
    let mut s = session();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1400.0, 900.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let t = ArrangerView::lane_tracks(&s)[0].id;
    let row = view.row_rect(0, size);
    let grip = Point::new(100.0, row.bottom() - 1.0);
    assert_eq!(
        view.hit_test(grip, size, &s),
        Some(Hit::Header(t, HeaderPart::Resize))
    );
    let mut actions = run(&mut view, down(grip), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(grip.x, grip.y + 50.0)), size, &s).0);
    actions.extend(run(&mut view, up(Point::new(grip.x, grip.y + 50.0)), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.track_height(t), Some(view.row_h() + 50.0));
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    assert_eq!(view.row_rect(0, size).h, view.row_h() + 50.0);
    assert_eq!(
        view.row_rect(1, size).y,
        row.y + view.row_h() + 50.0,
        "rows below move down"
    );

    // Alt+wheel changes every track.
    let ev = ViewEvent::Scroll {
        pos: Point::new(600.0, 300.0),
        dx: 0.0,
        dy: -2.0,
        modifiers: Modifiers {
            alt: true,
            ..Modifiers::NONE
        },
        precise: false,
    };
    let (a, _) = run(&mut view, ev, size, &s);
    let Some(Action::SetTrackHeight {
        track: None,
        height,
    }) = a.first().cloned()
    else {
        panic!("{a:?}");
    };
    s.dispatch(a[0].clone()).unwrap();
    let other = ArrangerView::lane_tracks(&s)[2].id;
    assert_eq!(s.track_height(other), Some(height));
    assert!(height > view.row_h());
}

#[test]
fn automation_lanes_show_and_edit_points() {
    use faderframe_session::{AutomationMode, AutomationTarget};
    let mut s = session();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1400.0, 900.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let t = ArrangerView::lane_tracks(&s)[0].id;
    // The header's A button shows the volume lane; the row grows.
    let l = view.header_layout(&s, t, size).unwrap();
    let (a, _) = run(&mut view, down(l.automation.center()), size, &s);
    assert_eq!(a, vec![Action::ToggleTrackAutomation(t)]);
    s.dispatch(a[0].clone()).unwrap();
    let lane = s.shown_lanes(t)[0].clone();
    assert_eq!(lane.target, AutomationTarget::TrackVolume);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.balanced_clips());
    let row = view.row_rect(0, size);
    assert_eq!(row.h, view.row_h() + AUTO_LANE_H);
    let lane_y = row.y + view.row_h() + AUTO_LANE_H * 0.5;

    // Click adds a point (and a first point holding the static value).
    let x = view.x_of(MusicalTime::from_quarters_i(2));
    let mut actions = run(&mut view, down(Point::new(x, lane_y)), size, &s).0;
    actions.extend(run(&mut view, up(Point::new(x, lane_y)), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let pts = s
        .project()
        .track(t)
        .unwrap()
        .automation
        .lane(AutomationTarget::TrackVolume)
        .unwrap()
        .curve
        .points()
        .to_vec();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[1].time, MusicalTime::from_quarters_i(2));
    assert!(
        pts[1].value < 0.0 && pts[1].value > -40.0,
        "mid-lane is below unity: {}",
        pts[1].value
    );

    // Dragging it up moves it to a later beat and a higher level; undo is one step.
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let y_now = {
        let row = view.row_rect(0, size);
        let area_bottom = row.y + view.row_h() + AUTO_LANE_H - 5.0;
        let area_h = AUTO_LANE_H - 10.0;
        let param = s
            .automation_param(t, AutomationTarget::TrackVolume)
            .unwrap();
        area_bottom - param.to_normal(pts[1].value) as f32 * area_h
    };
    let x2 = view.x_of(MusicalTime::from_quarters_i(3));
    let mut actions = run(&mut view, down(Point::new(x, y_now)), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(x + 10.0, y_now - 5.0)), size, &s).0);
    actions.extend(run(&mut view, mv(Point::new(x2, y_now - 10.0)), size, &s).0);
    actions.extend(run(&mut view, up(Point::new(x2, y_now - 10.0)), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let moved = s
        .project()
        .track(t)
        .unwrap()
        .automation
        .lane(AutomationTarget::TrackVolume)
        .unwrap()
        .curve
        .points()
        .to_vec();
    assert_eq!(moved[1].time, MusicalTime::from_quarters_i(3));
    assert!(moved[1].value > pts[1].value);
    s.dispatch(Action::Undo).unwrap();
    let back = s
        .project()
        .track(t)
        .unwrap()
        .automation
        .lane(AutomationTarget::TrackVolume)
        .unwrap()
        .curve
        .points()
        .to_vec();
    assert_eq!(back, pts);

    // The lane header's mode button opens the mode menu.
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let row = view.row_rect(0, size);
    let g = automation::LaneGeom {
        track: t,
        lane: lane.id,
        row: Rect::new(0.0, row.y + view.row_h(), size.w, AUTO_LANE_H),
    };
    let [_, (mode_r, _), _] = g.header_parts(view.header_w());
    let (_, req) = run(&mut view, down(mode_r.center()), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("mode menu expected");
    };
    let touch = items.iter().find(|i| i.label == "Touch").unwrap();
    s.dispatch(touch.action.clone().unwrap()).unwrap();
    assert_eq!(
        s.project()
            .track(t)
            .unwrap()
            .automation
            .lane(AutomationTarget::TrackVolume)
            .unwrap()
            .mode,
        AutomationMode::Touch
    );
}

#[test]
fn header_column_resizes_horizontally() {
    let mut s = session();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1400.0, 900.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let w0 = view.header_w();
    let edge = Point::new(w0, 200.0);
    assert_eq!(view.hit_test(edge, size, &s), Some(Hit::HeaderEdge));
    let mut actions = run(&mut view, down(edge), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(w0 + 80.0, 200.0)), size, &s).0);
    actions.extend(run(&mut view, up(Point::new(w0 + 80.0, 200.0)), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.header_width(), Some(w0 + 80.0));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert_eq!(view.header_w(), w0 + 80.0);
    // Lanes start after the wider header.
    assert!((view.x_of(MusicalTime::ZERO) - (w0 + 80.0)).abs() < 1e-3);
    let l = view
        .header_layout(&s, ArrangerView::lane_tracks(&s)[0].id, size)
        .unwrap();
    assert!(l.meter.right() <= w0 + 80.0);
}
