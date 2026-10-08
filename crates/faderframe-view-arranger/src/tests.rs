use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::ClipEdge;
use faderframe_ui_canvas::RecordingPainter;

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

/// The demo without its sections, marker, key, chords and folder (for
/// making them).
fn bare_session() -> Session {
    let config = EngineConfig::default();
    let mut p = faderframe_project::demo::demo_project(config.sample_rate);
    p.sections.clear();
    p.markers.clear();
    p.keys.clear();
    p.chords.clear();
    p.tracks.retain(|t| t.kind != TrackKind::Folder);
    for t in &mut p.tracks {
        t.folder = None;
    }
    Session::new(p, None, config).unwrap()
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
    // The lower half grabs (the upper half selects a range).
    let grab = Point::new(view.x_of(start) + 30.0, row.y + row.h * 0.8);

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
fn clips_dragged_out_go_back_and_another_view_can_take_them() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let tracks = ArrangerView::lane_tracks(&s);
    let bass_row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let bass = tracks[bass_row].id;
    let clip = s.project().track(bass).unwrap().clips[0];
    let start = s.project().clip(clip).unwrap().start;
    let row = view.row_rect(bass_row, size);
    let grab = Point::new(view.x_of(start) + 30.0, row.y + row.h * 0.8);
    let steps = s.history_steps().0.len();
    let mut actions = run(&mut view, down(grab), size, &s).0;
    assert!(view.drag_payload(&s).is_none(), "not before it moved");
    actions.extend(run(&mut view, mv(Point::new(grab.x + 40.0, grab.y)), size, &s).0);
    for a in actions.drain(..) {
        s.dispatch(a).unwrap();
    }
    assert_ne!(s.project().clip(clip).unwrap().start, start, "moving");
    // Below the arranger (over the launcher): the clip goes back while
    // it is carried there, and is offered.
    let below = Point::new(grab.x + 40.0, size.h + 50.0);
    for a in run(&mut view, mv(below), size, &s).0 {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.project().clip(clip).unwrap().start, start);
    let payload = view.drag_payload(&s).unwrap();
    assert_eq!(
        faderframe_session::launcher::parse_clips_payload(&payload),
        Some(vec![clip])
    );
    // Taken there: the move is cancelled, not an undo step.
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.cancel_drag(&mut cx);
    assert_eq!(actions, [Action::CancelGesture]);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(run(&mut view, up(below), size, &s).0.is_empty());
    assert_eq!(s.project().clip(clip).unwrap().start, start);
    assert_eq!(s.history_steps().0.len(), steps);
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
            rating: 0,
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

fn with_mods(ev: ViewEvent, m: Modifiers) -> ViewEvent {
    match ev {
        ViewEvent::PointerDown {
            pos,
            button,
            clicks,
            ..
        } => ViewEvent::PointerDown {
            pos,
            button,
            modifiers: m,
            clicks,
        },
        ViewEvent::PointerMove { pos, dragging, .. } => ViewEvent::PointerMove {
            pos,
            modifiers: m,
            dragging,
        },
        other => other,
    }
}

/// Press, move through `path`, release; dispatches everything.
fn drag(
    view: &mut ArrangerView,
    s: &mut Session,
    size: Size,
    from: Point,
    path: &[Point],
    m: Modifiers,
) -> Vec<Action> {
    let mut all = run(view, with_mods(down(from), m), size, s).0;
    for a in all.clone() {
        s.dispatch(a).unwrap();
    }
    let mut last = from;
    for p in path {
        let acts = run(view, with_mods(mv(*p), m), size, s).0;
        for a in &acts {
            s.dispatch(a.clone()).unwrap();
        }
        all.extend(acts);
        last = *p;
    }
    let acts = run(view, up(last), size, s).0;
    for a in &acts {
        s.dispatch(a.clone()).unwrap();
    }
    all.extend(acts);
    all
}

fn clip_of(s: &Session, track: &str) -> ClipId {
    let t = s.project().tracks.iter().find(|t| t.name == track).unwrap();
    t.clips[0]
}

#[test]
fn clicking_a_clip_selects_it_and_moves_the_playhead_to_the_snapped_click() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let clip = clip_of(&s, "Bass");
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let t = s.project().clip(clip).unwrap().start + MusicalTime::from_quarters(5.1);
    let at = Point::new(view.x_of(t), rect.y + rect.h * 0.8);
    let actions = drag(&mut view, &mut s, size, at, &[], Modifiers::NONE);
    assert!(actions.contains(&Action::SelectClips {
        clips: vec![clip],
        mode: SelectMode::Replace
    }));
    let expected = s.project().clip(clip).unwrap().start + MusicalTime::from_quarters_i(5);
    assert!(
        (s.playhead() - expected).quarters().abs() < 1e-3,
        "Grid mode snaps the click to the beat: {:?}",
        s.playhead()
    );
    // Slip mode: no snapping.
    s.dispatch(Action::SetEditMode(faderframe_session::EditMode::Slip))
        .unwrap();
    drag(&mut view, &mut s, size, at, &[], Modifiers::NONE);
    let off = (s.playhead() - t).quarters().abs();
    assert!(off < 0.05, "unsnapped: {off}");
}

#[test]
fn shift_click_selects_more_clips_and_edits_apply_to_all() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let (bass, pluck) = (clip_of(&s, "Bass"), clip_of(&s, "Pluck"));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let r1 = view.clip_view_rect(&s, size, bass).unwrap();
    let r2 = view.clip_view_rect(&s, size, pluck).unwrap();
    let lower = |r: Rect| Point::new(r.x + 60.0, r.y + r.h * 0.8);
    drag(&mut view, &mut s, size, lower(r1), &[], Modifiers::NONE);
    drag(&mut view, &mut s, size, lower(r2), &[], shift);
    assert_eq!(s.selection.clips.len(), 2, "shift adds to the selection");
    // Dragging one moves both, by the same (snapped) amount, as one step.
    let starts = |s: &Session| {
        (
            s.project().clip(bass).unwrap().start,
            s.project().clip(pluck).unwrap().start,
        )
    };
    let before = starts(&s);
    let from = lower(r1);
    let to = Point::new(from.x + 4.0 * view.ppq + 2.0, from.y);
    let actions = drag(
        &mut view,
        &mut s,
        size,
        from,
        &[Point::new(from.x + 8.0, from.y), to],
        Modifiers::NONE,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::MoveClips { clips, .. } if clips.len() == 2))
    );
    let after = starts(&s);
    let q4 = MusicalTime::from_quarters_i(4);
    assert_eq!(after, (before.0 + q4, before.1 + q4));
    assert_eq!(s.selection.clips.len(), 2, "a drag keeps the group");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(starts(&s), before);
    // Shift-clicking a selected clip removes it.
    drag(&mut view, &mut s, size, lower(r2), &[], shift);
    assert_eq!(s.selection.clips.len(), 1);
}

#[test]
fn edges_trim_handles_fade_and_the_badge_sets_clip_gain() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let clip = clip_of(&s, "Bass");
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let c = s.project().clip(clip).unwrap().clone();
    let pt = Point::new(rect.right() - 2.0, rect.y + rect.h * 0.7);
    assert_eq!(
        view.clip_zone(&s, &c, rect, pt),
        ClipZone::Trim(ClipEdge::End)
    );
    let end_before = c.end(&s.project().timeline, s.project().sample_rate);
    let left = Point::new(pt.x - 2.0 * view.ppq, pt.y);
    drag(
        &mut view,
        &mut s,
        size,
        pt,
        &[Point::new(pt.x - 6.0, pt.y), left],
        Modifiers::NONE,
    );
    let p_ = s.project();
    let end_after = p_.clip(clip).unwrap().end(&p_.timeline, p_.sample_rate);
    let want = end_before - MusicalTime::from_quarters_i(2);
    assert!(
        (end_after - want).quarters().abs() < 1e-3,
        "{end_after:?} vs {want:?}"
    );
    // The top corner draws a fade-in.
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.content_rect(rect);
    let corner = Point::new(rect.x + 3.0, content.y + 3.0);
    let c = s.project().clip(clip).unwrap().clone();
    assert!(matches!(
        view.clip_zone(&s, &c, rect, corner),
        ClipZone::Fade(ClipEdge::Start)
    ));
    let ppq = view.ppq;
    drag(
        &mut view,
        &mut s,
        size,
        corner,
        &[
            Point::new(corner.x + 10.0, corner.y),
            Point::new(corner.x + ppq, corner.y),
        ],
        Modifiers::NONE,
    );
    let ClipContent::Audio(a) = &s.project().clip(clip).unwrap().content else {
        panic!()
    };
    let quarter = s.project().sample_rate as i64 * 60 / 112;
    assert!(
        (a.fades.fade_in - quarter).abs() < quarter / 8,
        "fade ≈ a quarter: {}",
        a.fades.fade_in
    );
    // The gain readout drags clip gain.
    let badge = view.gain_badge(&c, rect).unwrap().center();
    let c = s.project().clip(clip).unwrap().clone();
    assert_eq!(view.clip_zone(&s, &c, rect, badge), ClipZone::Gain);
    drag(
        &mut view,
        &mut s,
        size,
        badge,
        &[
            Point::new(badge.x, badge.y - 10.0),
            Point::new(badge.x, badge.y - 40.0),
        ],
        Modifiers::NONE,
    );
    let ClipContent::Audio(a) = &s.project().clip(clip).unwrap().content else {
        panic!()
    };
    assert!(
        (a.gain_db - 4.0).abs() < 0.01,
        "40 px up = +4 dB: {}",
        a.gain_db
    );
}

#[test]
fn the_upper_half_selects_a_range_and_keys_switch_modes() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let clip = clip_of(&s, "Bass");
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.content_rect(rect);
    let start = s.project().clip(clip).unwrap().start;
    let a = Point::new(
        view.x_of(start + MusicalTime::from_quarters(1.0)),
        content.y + content.h * 0.3,
    );
    let b = Point::new(view.x_of(start + MusicalTime::from_quarters(3.0)), a.y);
    drag(
        &mut view,
        &mut s,
        size,
        a,
        &[Point::new(a.x + 10.0, a.y), b],
        Modifiers::NONE,
    );
    let r = s.selection.range.unwrap();
    assert_eq!(
        (r.start, r.end),
        (
            start + MusicalTime::from_quarters_i(1),
            start + MusicalTime::from_quarters_i(3)
        )
    );
    let key = |k: Key, m: Modifiers| ViewEvent::Key {
        key: k,
        modifiers: m,
    };
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    for a in run(&mut view, key(Key::Char('1'), alt), size, &s).0 {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.editor.edit_mode, faderframe_session::EditMode::Shuffle);
    let (a, _) = run(&mut view, key(Key::Char('b'), Modifiers::NONE), size, &s);
    assert_eq!(a, vec![Action::Separate]);
    let (a, _) = run(&mut view, key(Key::F(7), Modifiers::NONE), size, &s);
    assert_eq!(
        a,
        vec![Action::SetEditTool(faderframe_session::EditTool::Select)]
    );
}

#[test]
fn every_audio_clip_has_a_gain_knob_that_the_wheel_turns() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    // Each visible audio clip shows its gain.
    let audio_clips = s
        .project()
        .clips
        .values()
        .filter(|c| c.as_audio().is_some())
        .count();
    let readouts = p.texts().iter().filter(|t| **t == "0.0 dB").count();
    assert!(
        readouts >= 3 && readouts <= audio_clips,
        "{readouts} of {audio_clips}"
    );
    let clip = clip_of(&s, "Bass");
    let c = s.project().clip(clip).unwrap().clone();
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let knob = ArrangerView::gain_knob(view.gain_badge(&c, rect).unwrap());
    assert!(knob.w >= 16.0, "the name strip is tall enough for the knob");
    let wheel = ViewEvent::Scroll {
        pos: knob.center(),
        dx: 0.0,
        dy: -2.0,
        modifiers: Modifiers::NONE,
        precise: false,
    };
    let (a, _) = run(&mut view, wheel, size, &s);
    assert_eq!(
        a,
        vec![Action::ClipGain {
            clips: vec![clip],
            delta_db: 1.0
        }]
    );
    for a in a {
        s.dispatch(a).unwrap();
    }
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.texts().contains(&"+1.0 dB"));
    // MIDI clips have no gain knob.
    let melody = clip_of(&s, "Lead Synth");
    let m = s.project().clip(melody).unwrap().clone();
    let mrect = view.clip_view_rect(&s, size, melody).unwrap();
    assert!(view.gain_badge(&m, mrect).is_none());
}

#[test]
fn the_pencil_redraws_samples_when_zoomed_in_to_sample_level() {
    let mut s = session();
    s.dispatch(Action::SetEditTool(faderframe_session::EditTool::Pencil))
        .unwrap();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let clip = clip_of(&s, "Pluck");
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.clip_content_rect(rect);
    let half = content.h * 0.46;
    let from = Point::new(rect.x + 100.0, content.center().y - half * 0.5);
    let to = Point::new(rect.x + 260.0, content.center().y);

    // Not zoomed in: the Pencil leaves audio alone.
    let (acts, _) = run(&mut view, down(from), size, &s);
    run(&mut view, up(from), size, &s);
    assert!(!acts.iter().any(|a| matches!(a, Action::RedrawAudio { .. })));

    view.ppq = 200_000.0;
    view.scroll_x = 0.0;
    view.paint(&mut p, size, &s, &theme);
    assert!(view.sample_zoom(&s).is_some());
    let mut acts = run(&mut view, down(from), size, &s).0;
    for k in 1..=4 {
        let x = from.x + (to.x - from.x) * k as f32 / 4.0;
        let y = from.y + (to.y - from.y) * k as f32 / 4.0;
        acts.extend(run(&mut view, mv(Point::new(x, y)), size, &s).0);
    }
    view.paint(&mut p, size, &s, &theme);
    acts.extend(run(&mut view, up(to), size, &s).0);
    let Some(Action::RedrawAudio {
        clip: c,
        channel,
        start,
        samples,
    }) = acts
        .into_iter()
        .find(|a| matches!(a, Action::RedrawAudio { .. }))
    else {
        panic!("no redraw")
    };
    assert_eq!((c, channel), (clip, None), "mono: one lane, all channels");
    assert!(start > 0);
    // ~160 px at ~7.8 px per sample.
    assert!((18..=23).contains(&samples.len()), "{}", samples.len());
    assert!((samples[0] - 0.5).abs() < 0.02, "{}", samples[0]);
    assert!(samples.last().unwrap().abs() < 0.02);
    assert!(
        samples.windows(2).all(|w| w[1] <= w[0] + 1e-6),
        "a falling line"
    );
}

#[test]
fn global_lanes_add_markers_sections_and_change_the_tempo() {
    use faderframe_session::lanes::GlobalLane;
    let mut s = bare_session();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let lanes = view.global_lanes();
    assert_eq!(lanes.len(), 6);
    let lane = |l: GlobalLane| {
        let (_, y, h) = lanes.iter().copied().find(|(x, ..)| *x == l).unwrap();
        y + h / 2.0
    };
    let x_at = |view: &ArrangerView, q: f64| view.x_of(MusicalTime::from_quarters(q));

    // Double-click on the markers lane.
    let at = Point::new(x_at(&view, 8.0), lane(GlobalLane::Markers));
    let dbl = ViewEvent::PointerDown {
        pos: at,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks: 2,
    };
    let (acts, _) = run(&mut view, dbl, size, &s);
    assert_eq!(
        acts,
        vec![Action::AddMarker(MusicalTime::from_quarters(8.0))]
    );
    run(&mut view, up(at), size, &s);
    for a in acts {
        s.dispatch(a).unwrap();
    }
    view.paint(&mut p, size, &s, &theme);
    assert!(matches!(
        view.hit_test(Point::new(at.x + 6.0, at.y), size, &s),
        Some(Hit::Global(GlobalHit::Marker(_)))
    ));

    // Drag across the arranger lane: a section snapped to the grid.
    let y = lane(GlobalLane::Arranger);
    let (a, b, c) = (x_at(&view, 4.1), x_at(&view, 12.0), x_at(&view, 16.1));
    let acts = drag(
        &mut view,
        &mut s,
        size,
        Point::new(a, y),
        &[Point::new(b, y), Point::new(c, y)],
        Modifiers::NONE,
    );
    assert!(acts.contains(&Action::AddSection {
        start: MusicalTime::from_quarters(4.0),
        end: MusicalTime::from_quarters(16.0),
    }));
    assert_eq!(s.project().sections.len(), 1);
    assert_eq!(s.project().sections[0].name, "Intro");

    // Drag the first tempo point up.
    view.paint(&mut p, size, &s, &theme);
    let bpm = s.project().timeline.tempo.points()[0].bpm;
    let (_, ty, th) = lanes
        .iter()
        .copied()
        .find(|(x, ..)| *x == GlobalLane::Tempo)
        .unwrap();
    let lane_rect = Rect::new(view.header_w(), ty, size.w - view.header_w(), th);
    let point_y = ArrangerView::tempo_y_for_test(lane_rect, &s, bpm);
    let from = Point::new(x_at(&view, 0.0), point_y);
    assert_eq!(
        view.hit_test(from, size, &s),
        Some(Hit::Global(GlobalHit::Tempo(0)))
    );
    let acts = drag(
        &mut view,
        &mut s,
        size,
        from,
        &[
            Point::new(from.x, from.y - 10.0),
            Point::new(from.x + 1.0, from.y - 20.0),
        ],
        Modifiers::NONE,
    );
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SetTempoPoint { index: 0, .. }))
    );
    assert!(s.project().timeline.tempo.points()[0].bpm > bpm + 5.0);

    // The lane title shows or hides lanes.
    let label = Point::new(20.0, lane(GlobalLane::Signature));
    let (_, req) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: label,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        size,
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("lanes menu")
    };
    // Every lane, Video and Lyrics too (shown once there are some).
    assert_eq!(items.len(), 8);
}

#[test]
fn the_chord_and_key_lanes_take_typed_chords_moves_and_keys() {
    use faderframe_project::harmony::{Chord, Key, Scale};
    use faderframe_session::lanes::GlobalLane;
    let mut s = bare_session();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let lanes = view.global_lanes();
    let lane = |l: GlobalLane| {
        let (_, y, h) = lanes.iter().copied().find(|(x, ..)| *x == l).unwrap();
        y + h / 2.0
    };
    let x_at = |view: &ArrangerView, q: f64| view.x_of(MusicalTime::from_quarters(q));
    let q = MusicalTime::from_quarters;

    // Drag across the chord lane over bar 2, then type the chord.
    let y = lane(GlobalLane::Chords);
    let (a, b) = (
        Point::new(x_at(&view, 4.05), y),
        Point::new(x_at(&view, 8.0), y),
    );
    run(&mut view, down(a), size, &s);
    run(&mut view, mv(b), size, &s);
    let (_, req) = run(&mut view, up(b), size, &s);
    let Some(HostRequest::TextInput { commit, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("type the chord")
    };
    assert!(commit("H9").is_none(), "not a chord");
    s.dispatch(commit("Am7").unwrap()).unwrap();
    let c = s.project().chords[0];
    assert_eq!(
        (c.start, c.end, c.chord),
        (q(4.0), q(8.0), Chord::parse("Am7").unwrap())
    );

    // Drag it a bar later by its body.
    view.paint(&mut p, size, &s, &theme);
    let body = Point::new(x_at(&view, 6.0), y);
    assert!(matches!(
        view.hit_test(body, size, &s),
        Some(Hit::Global(GlobalHit::Chord(0, _)))
    ));
    let (x8, x10) = (x_at(&view, 8.0), x_at(&view, 10.0));
    drag(
        &mut view,
        &mut s,
        size,
        body,
        &[Point::new(x8, y), Point::new(x10, y)],
        Modifiers::NONE,
    );
    let c = s.project().chords[0];
    assert_eq!((c.start, c.end), (q(8.0), q(12.0)));

    // Its menu offers the key's chords: set the key first (C major).
    let ky = lane(GlobalLane::Key);
    let at = Point::new(x_at(&view, 1.0), ky);
    let (_, req) = run(&mut view, down(at), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("the key menu")
    };
    let c_major = items
        .iter()
        .find(|i| i.label == "C Major")
        .and_then(|i| i.action.clone())
        .unwrap();
    s.dispatch(c_major).unwrap();
    assert_eq!(s.project().key_at(q(0.0)), Some(Key::new(0, Scale::Major)));
    view.paint(&mut p, size, &s, &theme);
    let req = view.global_menu(
        GlobalHit::Chord(0, crate::global::SectionPart::Body),
        &s,
        body,
    );
    let HostRequest::ContextMenu { items, .. } = req else {
        panic!("the chord menu")
    };
    assert!(items[0].label.contains("vi7"), "{}", items[0].label);
    let f = items
        .iter()
        .find(|i| i.label.starts_with("F "))
        .and_then(|i| i.action.clone())
        .unwrap();
    s.dispatch(f).unwrap();
    assert_eq!(s.project().chords[0].chord.name(false), "F");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().chords[0].chord.name(false), "Am7");
}

#[test]
fn both_loop_edges_resize_without_toggling_and_undo_as_one_gesture() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let q = MusicalTime::from_quarters_i;
    let range = MusicalRange::new(q(4), q(12)).unwrap();
    s.dispatch(Action::Transport(TransportAction::SetLoop(Some(range))))
        .unwrap();
    let enabled = s.project().loop_enabled;
    for (left, target) in [(true, 2), (true, 6), (false, 16), (false, 8)] {
        let from = Point::new(view.x_of(if left { range.start } else { range.end }), 5.0);
        let to = Point::new(view.x_of(q(target)), 5.0);
        assert_eq!(
            view.hit_test(from, size, &s),
            Some(if left { Hit::LoopStart } else { Hit::LoopEnd })
        );
        for ev in [
            down(from),
            mv(Point::new((from.x + to.x) / 2.0, 5.0)),
            mv(to),
            up(to),
        ] {
            for action in run(&mut view, ev, size, &s).0 {
                s.dispatch(action).unwrap();
            }
        }
        let resized = s.project().loop_range.unwrap();
        assert_eq!(
            Some(resized),
            if left {
                MusicalRange::new(q(target), range.end)
            } else {
                MusicalRange::new(range.start, q(target))
            }
        );
        assert_eq!(s.project().loop_enabled, enabled);
        s.dispatch(Action::Undo).unwrap();
        assert_eq!(s.project().loop_range, Some(range));
    }
    let from = Point::new(view.x_of(range.start), 5.0);
    for ev in [down(from), up(from)] {
        for action in run(&mut view, ev, size, &s).0 {
            s.dispatch(action).unwrap();
        }
    }
    assert_eq!(s.project().loop_enabled, enabled);
    // Edges cannot cross; the untouched end stays fixed.
    let to = Point::new(view.x_of(q(20)), 5.0);
    for ev in [down(from), mv(to), up(to)] {
        for action in run(&mut view, ev, size, &s).0 {
            s.dispatch(action).unwrap();
        }
    }
    let resized = s.project().loop_range.unwrap();
    assert_eq!(resized.end, range.end);
    assert!(resized.start < resized.end);
}

#[test]
fn the_plus_under_the_last_track_offers_new_tracks() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    // Tall enough for every track and the button.
    let size = Size::new(1400.0, 1600.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.texts().contains(&"+  Add Track"));
    let count = ArrangerView::lane_tracks(&s).len();
    let plus = view.add_track_rect(count, size);
    assert!(plus.y >= view.row_rect(count - 1, size).bottom());
    let (_, req) = run(&mut view, down(plus.center()), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu")
    };
    let midi = items
        .iter()
        .find(|i| i.label == "MIDI Track")
        .and_then(|i| i.action.clone())
        .unwrap();
    s.dispatch(midi).unwrap();
    assert_eq!(ArrangerView::lane_tracks(&s).len(), count + 1);
}

#[test]
fn midi_tracks_have_no_hidden_fader() {
    let mut s = session();
    let midi = s.add_track(TrackKind::Midi).unwrap();
    let audio = ArrangerView::lane_tracks(&s)[0].id;
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 1400.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    for (track, fader) in [(audio, true), (midi, false)] {
        let l = view.header_layout(&s, track, size).unwrap();
        let hit = view.hit_test(l.volume.center(), size, &s);
        assert_eq!(
            hit == Some(Hit::Header(track, HeaderPart::Volume)),
            fader,
            "{hit:?}"
        );
        let meter = view.hit_test(l.meter.center(), size, &s);
        assert_eq!(meter == Some(Hit::Header(track, HeaderPart::Meter)), fader);
    }
    // The wheel over where a fader would be changes nothing.
    let l = view.header_layout(&s, midi, size).unwrap();
    let (actions, _) = run(
        &mut view,
        ViewEvent::Scroll {
            pos: l.volume.center(),
            dx: 0.0,
            dy: 3.0,
            precise: false,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Edit(Command::SetTrackVolume { .. }))),
        "{actions:?}"
    );
}

#[test]
fn tracks_are_sized_by_the_edges_of_their_headers() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 1400.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let tracks: Vec<TrackId> = ArrangerView::lane_tracks(&s).iter().map(|t| t.id).collect();
    let (first, second) = (tracks[0], tracks[1]);
    let h = view.base_h(&s, first);
    let edge = view.row_rect(0, size).y + h;
    // Both sides of the boundary size the upper track, in the headers only.
    for dy in [-3.0, 0.0, 3.0] {
        assert_eq!(
            view.hit_test(Point::new(40.0, edge + dy), size, &s),
            Some(Hit::Header(first, HeaderPart::Resize)),
            "{dy}"
        );
    }
    assert_ne!(
        view.hit_test(Point::new(40.0, edge + 10.0), size, &s),
        Some(Hit::Header(first, HeaderPart::Resize))
    );
    assert_ne!(
        view.hit_test(Point::new(view.header_w() + 40.0, edge), size, &s),
        Some(Hit::Header(first, HeaderPart::Resize))
    );
    // Dragging the top edge of the second header down makes the first
    // taller; it lights while hovered.
    let at = Point::new(40.0, edge + 2.0);
    run(&mut view, mv(at), size, &s);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    run(&mut view, down(at), size, &s);
    let (actions, _) = run(&mut view, mv(Point::new(40.0, edge + 32.0)), size, &s);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    run(&mut view, up(Point::new(40.0, edge + 32.0)), size, &s);
    assert_eq!(s.track_height(first), Some(h + 30.0));
    assert_eq!(s.track_height(second), None, "only the dragged one");
    // With automation lanes shown, the edge is still the track's own.
    s.dispatch(Action::ToggleTrackAutomation(first)).unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let edge = view.row_rect(0, size).y + view.base_h(&s, first);
    assert_eq!(
        view.hit_test(Point::new(40.0, edge), size, &s),
        Some(Hit::Header(first, HeaderPart::Resize))
    );
}

#[test]
fn folders_indent_their_tracks_and_close_by_their_triangle() {
    let mut s = bare_session();
    let ids = |s: &Session| -> Vec<TrackId> {
        ArrangerView::lane_tracks(s).iter().map(|t| t.id).collect()
    };
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    s.dispatch(Action::NewFolder { tracks: vec![bass] })
        .unwrap();
    let folder = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Folder)
        .unwrap()
        .id;
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 1400.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.texts().contains(&"Folder 1"));
    let order = ids(&s);
    let at = order.iter().position(|t| *t == folder).unwrap();
    assert_eq!(order[at + 1], bass, "the folder's track right under it");
    // The track's header is indented.
    let fl = view.header_layout(&s, folder, size).unwrap();
    let bl = view.header_layout(&s, bass, size).unwrap();
    assert!(bl.row.x > fl.row.x);
    // A folder header has mute and solo, no record or fader.
    for (r, part) in [
        (bl.record, HeaderPart::Record),
        (bl.volume, HeaderPart::Volume),
    ] {
        let _ = part;
        let on_folder = Point::new(r.center().x, fl.row.y + (r.center().y - bl.row.y));
        assert!(!matches!(
            view.hit_test(on_folder, size, &s),
            Some(Hit::Header(_, HeaderPart::Record | HeaderPart::Volume))
        ));
    }
    // Its triangle closes it: the track is gone from the lanes.
    let fold = fold_rect(&fl);
    assert_eq!(
        view.hit_test(fold.center(), size, &s),
        Some(Hit::Header(folder, HeaderPart::Fold))
    );
    let (actions, _) = run(&mut view, down(fold.center()), size, &s);
    assert!(actions.contains(&Action::ToggleFolder(folder)));
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(!ids(&s).contains(&bass));
}

#[test]
fn the_wheel_over_the_track_headers_never_scrolls() {
    let s = session();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    // Short: the tracks can scroll.
    let size = Size::new(1200.0, 260.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let wheel = |pos: Point| ViewEvent::Scroll {
        pos,
        dx: 0.0,
        dy: 3.0,
        modifiers: Modifiers::NONE,
        precise: false,
    };
    let l = view.header_layout(&s, bass, size).unwrap();
    // Over a header, not on a control: nothing moves.
    let row = view.row_rect(0, size);
    let (a, _) = run(&mut view, wheel(Point::new(8.0, row.y + 4.0)), size, &s);
    assert!(a.is_empty());
    assert_eq!(view.scroll_y, 0.0);
    // On its fader: the fader moves, the tracks stay.
    let (a, _) = run(&mut view, wheel(l.volume.center()), size, &s);
    assert!(
        matches!(a.as_slice(), [Action::Edit(Command::SetTrackVolume { .. })]),
        "{a:?}"
    );
    assert_eq!(view.scroll_y, 0.0);
    // Over the lanes it scrolls.
    run(&mut view, wheel(Point::new(700.0, row.y + 4.0)), size, &s);
    assert!(view.scroll_y > 0.0);
}

#[test]
fn a_held_scrollbar_keeps_the_view_from_following_the_playhead() {
    let mut s = session();
    s.start_audio(
        vec![Box::new(faderframe_audio::dummy::DummyBackend::default())],
        &faderframe_session::AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::SetEditFlag(
        faderframe_session::EditFlag::FollowPlayhead,
        true,
    ))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let start = std::time::Instant::now();
    while !s.transport().playing && start.elapsed().as_secs() < 5 {
        s.tick(0.016);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(s.transport().playing);
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let size = Size::new(1200.0, 400.0);
    // Dragged far from the playhead while held: it stays there.
    view.scroll_held(ScrollAxis::Horizontal, true);
    view.set_scroll(ScrollAxis::Horizontal, 1_500.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    assert_eq!(view.scroll_x, 1_500.0);
    // Let go: it follows again.
    view.scroll_held(ScrollAxis::Horizontal, false);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    assert!(view.scroll_x < 1_500.0, "{}", view.scroll_x);
}

/// A launcher clip dragged over the arranger shows where it goes and is
/// dropped as a copy at the track and (snapped) time under the pointer;
/// arrangement clips are not taken this way (they move in the arranger's
/// own drag).
#[test]
fn launcher_clips_drop_into_the_arrangement() {
    let mut s = session();
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    let first = s.project().clips_of(drums)[0].id;
    s.dispatch(Action::Launcher(
        faderframe_session::launcher::LauncherOp::SendClips(vec![first]),
    ))
    .unwrap();
    let scene = s.project().launcher.scenes[0].id;
    let slot = s.project().launcher.clip(drums, scene).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let row = ArrangerView::lane_tracks(&s)
        .iter()
        .position(|t| t.id == drums)
        .unwrap();
    let y = view.row_rect(row, size).center().y;
    let at = MusicalTime::from_quarters(32.0);
    let pos = Point::new(view.x_of(at) + 2.0, y);
    let payload = faderframe_session::launcher::clips_payload(&[slot]);
    assert!(view.hover_payload(Some((&payload, pos)), size, &s));
    assert!(view.drop_at.is_some());
    let action = view.drop_payload(&payload, pos, size, &s).unwrap();
    let Action::Launcher(faderframe_session::launcher::LauncherOp::ToArrangement {
        clips,
        track,
        at: dropped,
    }) = action
    else {
        panic!("{action:?}")
    };
    assert_eq!((clips, track), (vec![slot], drums));
    assert!((dropped.quarters() - 32.0).abs() < 1.0, "{dropped:?}");
    // The arrangement's own clips: not this way.
    let own = faderframe_session::launcher::clips_payload(&[first]);
    assert!(view.drop_payload(&own, pos, size, &s).is_none());
}

/// A click in a clip's header strip selects it and leaves the playhead
/// where it is; a click in its body (Link Timeline) still moves it there.
#[test]
fn a_click_on_a_clips_header_does_not_move_the_playhead() {
    let mut s = session();
    s.editor.link_timeline = true;
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let tracks = ArrangerView::lane_tracks(&s);
    let bass_row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let bass = tracks[bass_row].id;
    let clip = s.project().track(bass).unwrap().clips[0];
    let start = s.project().clip(clip).unwrap().start;
    let row = view.row_rect(bass_row, size);
    let x = view.x_of(start) + 40.0;
    let located = |a: &[Action]| {
        a.iter()
            .any(|a| matches!(a, Action::Transport(TransportAction::Locate(_))))
    };
    // The header strip: the clip's top few pixels.
    let header = Point::new(x, row.y + 4.0);
    let mut a = run(&mut view, down(header), size, &s).0;
    a.extend(run(&mut view, up(header), size, &s).0);
    assert!(a.contains(&Action::SelectClips {
        clips: vec![clip],
        mode: SelectMode::Replace
    }));
    assert!(!located(&a), "a header click keeps the playhead: {a:?}");
    // The lower half: the playhead goes to the click.
    let body = Point::new(x, row.y + row.h * 0.8);
    let mut a = run(&mut view, down(body), size, &s).0;
    a.extend(run(&mut view, up(body), size, &s).0);
    assert!(located(&a), "{a:?}");
}

/// Track headers drag: between rows they reorder (one `PlaceTrack`), onto
/// a folder's middle they go into it, and a click only selects.
#[test]
fn track_headers_drag_to_reorder_and_into_folders() {
    let mut s = bare_session();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let theme = Theme::default();
    let paint = |view: &mut ArrangerView, s: &Session| {
        let mut p = RecordingPainter::default();
        view.paint(&mut p, size, s, &theme);
    };
    paint(&mut view, &s);
    let ids = |s: &Session| -> Vec<TrackId> {
        ArrangerView::lane_tracks(s).iter().map(|t| t.id).collect()
    };
    let start = ids(&s);
    let (t0, t2, t3) = (start[0], start[2], start[3]);
    let grab = view.header_layout(&s, t0, size).unwrap().name.center();
    // Into the lower half of the third row: between it and the fourth.
    let row = |view: &ArrangerView, s: &Session, id| {
        let i = ids(s).iter().position(|t| *t == id).unwrap();
        view.row_rect(i, size)
    };
    let target = row(&view, &s, t2);
    let drop = Point::new(grab.x, target.y + target.h * 0.8);
    let mut actions = run(&mut view, down(grab), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(grab.x, grab.y + 10.0)), size, &s).0);
    actions.extend(run(&mut view, mv(drop), size, &s).0);
    actions.extend(run(&mut view, up(drop), size, &s).0);
    assert!(
        actions.contains(&Action::PlaceTrack {
            track: t0,
            after: Some(t2),
            before: Some(t3),
        }),
        "{actions:?}"
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let now = ids(&s);
    assert_eq!(&now[..3], &[start[1], t2, t0]);
    // A folder holding the fourth track; the second dragged onto it.
    s.dispatch(Action::NewFolder { tracks: vec![t3] }).unwrap();
    paint(&mut view, &s);
    let folder = ArrangerView::lane_tracks(&s)
        .iter()
        .find(|t| t.kind == TrackKind::Folder)
        .unwrap()
        .id;
    let mover = start[1];
    let grab = view.header_layout(&s, mover, size).unwrap().name.center();
    let f = row(&view, &s, folder);
    let onto = Point::new(grab.x, f.y + f.h * 0.5);
    let mut actions = run(&mut view, down(grab), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(grab.x, grab.y + 10.0)), size, &s).0);
    actions.extend(run(&mut view, mv(onto), size, &s).0);
    actions.extend(run(&mut view, up(onto), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.project().track(mover).unwrap().folder, Some(folder));
    // A click: selected, nothing moves.
    let before = ids(&s);
    let at = view.header_layout(&s, t2, size).unwrap().name.center();
    let mut actions = run(&mut view, down(at), size, &s).0;
    actions.extend(run(&mut view, up(at), size, &s).0);
    assert!(
        actions
            .iter()
            .all(|a| matches!(a, Action::SelectTracks { .. })),
        "{actions:?}"
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(ids(&s), before);
}

/// Slip (free) moves land on whole samples with Snap to Samples (at the
/// demo's 112 BPM a free tick position rarely is one).
#[test]
fn free_moves_land_on_whole_samples() {
    let mut s = session();
    s.dispatch(Action::SetEditMode(faderframe_session::EditMode::Slip))
        .unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let tracks = ArrangerView::lane_tracks(&s);
    let bass_row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let clip = s.project().track(tracks[bass_row].id).unwrap().clips[0];
    let start = s.project().clip(clip).unwrap().start;
    let row = view.row_rect(bass_row, size);
    let grab = Point::new(view.x_of(start) + 30.0, row.y + row.h * 0.8);
    let target = Point::new(grab.x + 37.3, grab.y);
    let mut actions = run(&mut view, down(grab), size, &s).0;
    actions.extend(run(&mut view, mv(Point::new(grab.x + 10.0, grab.y)), size, &s).0);
    actions.extend(run(&mut view, mv(target), size, &s).0);
    actions.extend(run(&mut view, up(target), size, &s).0);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let moved = s.project().clip(clip).unwrap().start;
    assert_ne!(moved, start);
    let tl = &s.project().timeline;
    let rate = f64::from(s.project().sample_rate);
    assert_eq!(tl.to_musical(tl.to_samples(moved, rate), rate), moved);
}
