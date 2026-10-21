//! Snap to Objects (issue #7) and the grid at every zoom in the arranger.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::{EditFlag, EditMode, EditTool, SnapObject};
use faderframe_ui_canvas::{DrawOp, RecordingPainter};

/// The demo without its sections and marker (fewer objects to reason
/// about).
fn bare() -> Session {
    let config = EngineConfig::default();
    let mut p = faderframe_project::demo::demo_project(config.sample_rate);
    p.sections.clear();
    p.markers.clear();
    Session::new(p, None, config).unwrap()
}

const SIZE: Size = Size::new(1400.0, 700.0);

fn with_mods(ev: ViewEvent, mods: Modifiers) -> ViewEvent {
    match ev {
        ViewEvent::PointerDown {
            pos,
            button,
            clicks,
            ..
        } => ViewEvent::PointerDown {
            pos,
            button,
            modifiers: mods,
            clicks,
        },
        ViewEvent::PointerMove { pos, dragging, .. } => ViewEvent::PointerMove {
            pos,
            modifiers: mods,
            dragging,
        },
        ViewEvent::PointerUp { pos, button, .. } => ViewEvent::PointerUp {
            pos,
            button,
            modifiers: mods,
        },
        other => other,
    }
}

fn run(view: &mut ArrangerView, ev: ViewEvent, s: &Session) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    actions
}

/// A drag from `from` to `to` (through a point on the way), its actions
/// dispatched; the guide seen just before the release.
fn drag(
    view: &mut ArrangerView,
    s: &mut Session,
    from: Point,
    to: Point,
    mods: Modifiers,
) -> Option<faderframe_session::SnapHit> {
    let down = ViewEvent::PointerDown {
        pos: from,
        button: PointerButton::Primary,
        modifiers: mods,
        clicks: 1,
    };
    let mut actions = run(view, down, s);
    for pos in [Point::new((from.x + to.x) * 0.5, from.y), to] {
        let ev = ViewEvent::PointerMove {
            pos,
            modifiers: mods,
            dragging: true,
        };
        actions.extend(run(view, with_mods(ev, mods), s));
    }
    let guide = view.snap_guide;
    let up = ViewEvent::PointerUp {
        pos: to,
        button: PointerButton::Primary,
        modifiers: mods,
    };
    actions.extend(run(view, up, s));
    assert_eq!(view.snap_guide, None, "the guide goes with the drag");
    for a in actions {
        s.dispatch(a).unwrap();
    }
    guide
}

/// The Bass clip (row, id, start, end) and the nearest clip edge on
/// another track at least two quarters right of its start.
fn bass_and_target(s: &Session) -> (usize, ClipId, MusicalTime, MusicalTime) {
    let tracks = ArrangerView::lane_tracks(s);
    let row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let bass = tracks[row].id;
    let clip = s.project().track(bass).unwrap().clips[0];
    let start = s.project().clip(clip).unwrap().start;
    let p = s.project();
    let target = tracks
        .iter()
        .filter(|t| t.id != bass)
        .flat_map(|t| p.clips_of(t.id))
        .flat_map(|c| [c.start, c.end(&p.timeline, p.sample_rate)])
        .filter(|&t| t > start + MusicalTime::from_quarters(2.0))
        .min()
        .unwrap();
    (row, clip, start, target)
}

/// Slip: a clip dragged to within a few pixels of another track's clip
/// edge lands exactly on it (shown by the guide); Alt, or the option off,
/// leaves it where it was dropped.
#[test]
fn a_dragged_clip_snaps_to_another_tracks_clip_edge() {
    for (mods, on, snaps) in [
        (Modifiers::NONE, true, true),
        (
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
            true,
            false,
        ),
        (Modifiers::NONE, false, false),
    ] {
        let mut s = bare();
        s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
        s.dispatch(Action::SetEditFlag(EditFlag::SnapToObjects, on))
            .unwrap();
        let mut view = ArrangerView::new(Theme::default());
        let (row, clip, start, target) = bass_and_target(&s);
        let r = view.row_rect(row, SIZE);
        let grab = Point::new(view.x_of(start) + 30.0, r.y + r.h * 0.8);
        // The clip's start 4 px right of the target.
        let to = Point::new(view.x_of(target) + 30.0 + 4.0, grab.y);
        let guide = drag(&mut view, &mut s, grab, to, mods);
        let moved = s.project().clip(clip).unwrap().start;
        if snaps {
            assert_eq!(moved, target, "onto the other track's clip edge");
            let g = guide.unwrap();
            assert_eq!(g.at, target);
            assert!(matches!(
                g.object,
                SnapObject::ClipStart(_) | SnapObject::ClipEnd(_)
            ));
        } else {
            assert_eq!(guide, None);
            let px = (moved - target).quarters() * f64::from(view.ppq);
            assert!((px - 4.0).abs() < 0.5, "free: {px} px past the edge");
        }
    }
}

/// Grid mode: an object off the grid within reach beats the grid; out of
/// reach the grid decides as before.
#[test]
fn in_grid_mode_an_object_off_the_grid_beats_the_grid() {
    let mut s = bare();
    let mut view = ArrangerView::new(Theme::default());
    let (row, clip, start, _) = bass_and_target(&s);
    // A clip on another track, moved off the grid.
    let tracks = ArrangerView::lane_tracks(&s);
    let other = tracks
        .iter()
        .filter(|t| t.name != "Bass")
        .find_map(|t| t.clips.first().copied())
        .unwrap();
    s.dispatch(Action::MoveClips {
        clips: vec![other],
        by: MusicalTime::from_quarters(16.37).ticks(),
        tracks: 0,
    })
    .unwrap();
    let off = s.project().clip(other).unwrap().start;
    let meter = &s.project().timeline.meter;
    assert_ne!(s.editor.snap(off, meter), off, "off the grid");
    let r = view.row_rect(row, SIZE);
    let grab = Point::new(view.x_of(start) + 30.0, r.y + r.h * 0.8);
    let to = Point::new(view.x_of(off) + 30.0 + 3.0, grab.y);
    let guide = drag(&mut view, &mut s, grab, to, Modifiers::NONE);
    assert_eq!(s.project().clip(clip).unwrap().start, off);
    assert_eq!(guide.map(|g| g.object), Some(SnapObject::ClipStart(other)));
    // Out of reach (20 px): on the grid.
    s.dispatch(Action::Undo).unwrap();
    let grab = Point::new(view.x_of(start) + 30.0, r.y + r.h * 0.8);
    let to = Point::new(view.x_of(off) + 30.0 + 20.0, grab.y);
    drag(&mut view, &mut s, grab, to, Modifiers::NONE);
    let moved = s.project().clip(clip).unwrap().start;
    assert_eq!(s.editor.snap(moved, &s.project().timeline.meter), moved);
    assert_ne!(moved, off);
}

/// A trim snaps the trimmed edge; a range selection its moving edge (and
/// never to the clip being trimmed).
#[test]
fn trims_and_ranges_snap_their_edge_to_objects() {
    let mut s = bare();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let (row, clip, start, target) = bass_and_target(&s);
    let p = s.project();
    let end = p.clip(clip).unwrap().end(&p.timeline, p.sample_rate);
    let r = view.row_rect(row, SIZE);
    // Trim the end (Trim tool: the right half trims the end) to 5 px short
    // of an object left of it — the clip's own edges never attract.
    s.dispatch(Action::SetEditTool(EditTool::Trim)).unwrap();
    let target_left = ArrangerView::lane_tracks(&s)
        .iter()
        .filter(|t| t.name != "Bass")
        .flat_map(|t| s.project().clips_of(t.id))
        .flat_map(|c| {
            [
                c.start,
                c.end(&s.project().timeline, s.project().sample_rate),
            ]
        })
        .filter(|&t| t > start + MusicalTime::from_quarters(1.0) && t < end)
        .max();
    assert!(
        target_left.is_some(),
        "an edge over the Bass clip to trim to"
    );
    if let Some(t) = target_left {
        let from = Point::new(view.x_of(end) - 3.0, r.y + r.h * 0.8);
        let to = Point::new(view.x_of(t) + 5.0, from.y);
        let guide = drag(&mut view, &mut s, from, to, Modifiers::NONE);
        let p = s.project();
        // Audio ends on a whole sample: the object's, rounded.
        let rate = f64::from(p.sample_rate);
        let trimmed = p.clip(clip).unwrap().end(&p.timeline, p.sample_rate);
        let off = p.timeline.to_samples(trimmed, rate) - p.timeline.to_samples(t, rate);
        assert!(off.abs() <= 1, "{off} samples from the object");
        assert_eq!(guide.map(|g| g.at), Some(t));
        s.dispatch(Action::Undo).unwrap();
    }
    // A range from inside the clip to 4 px before another track's edge.
    s.dispatch(Action::SetEditTool(EditTool::Select)).unwrap();
    let from = Point::new(view.x_of(start) + 40.0, r.y + r.h * 0.5);
    let to = Point::new(view.x_of(target) - 4.0, from.y);
    let guide = drag(&mut view, &mut s, from, to, Modifiers::NONE);
    let range = s.selection.range.unwrap();
    assert_eq!(range.end, target);
    assert_eq!(guide.map(|g| g.at), Some(target));
}

/// While a drag snaps, the guide and the object's name are painted.
#[test]
fn the_guide_line_names_the_object() {
    let mut s = bare();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let (row, _, start, target) = bass_and_target(&s);
    let r = view.row_rect(row, SIZE);
    let grab = Point::new(view.x_of(start) + 30.0, r.y + r.h * 0.8);
    run(
        &mut view,
        ViewEvent::PointerDown {
            pos: grab,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &s,
    );
    let to = Point::new(view.x_of(target) + 33.0, grab.y);
    for a in run(
        &mut view,
        ViewEvent::PointerMove {
            pos: to,
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        &s,
    ) {
        s.dispatch(a).unwrap();
    }
    let theme = Theme::default();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &theme);
    let texts = p.texts();
    assert!(
        texts.contains(&"Clip Start") || texts.contains(&"Clip End"),
        "{texts:?}"
    );
}

/// The grid shows at every zoom — down to single samples — and its lines
/// are where clips snap: a clip dragged in Grid mode lands on a drawn
/// line, every subdivision drawn is a snap position.
#[test]
fn the_grid_shows_at_every_zoom_and_sits_where_clips_snap() {
    let s = Session::demo(EngineConfig::default()).unwrap();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    let lanes_w = SIZE.w - view.header_w();
    for ppq in [MIN_PPQ, 4.0, 34.0, 300.0, 3_000.0, 40_000.0, MAX_PPQ] {
        view.ppq = ppq;
        view.scroll_x = 0.0;
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &theme);
        let top = view.ruler_h();
        let full = SIZE.h - top;
        let lines = p
            .ops
            .iter()
            .filter(|o| {
                matches!(o, DrawOp::Rect(r)
                    if r.w <= 2.0 && (r.y - top).abs() < 0.01 && (r.h - full).abs() < 0.01)
            })
            .count();
        assert!(
            lines as f32 >= lanes_w / 40.0,
            "{ppq} px/quarter: {lines} grid lines"
        );
        assert!(lines as f32 <= 2.0 * lanes_w / 8.0 + 8.0, "{ppq}: {lines}");
    }
    // Subdivisions drawn are snap positions.
    let mut s = s;
    s.dispatch(Action::SetGrid(GridDivision::Triplet(16)))
        .unwrap();
    view.ppq = 300.0;
    let lines = view.grid_lines(MusicalTime::ZERO, MusicalTime::from_quarters(8.0), &s);
    let meter = s.project().timeline.meter.clone();
    assert!(lines.iter().any(|l| l.1 == GridLineKind::Subdivision));
    for &(t, k) in &lines {
        match k {
            GridLineKind::Subdivision => assert_eq!(s.editor.snap(t, &meter), t),
            GridLineKind::Fine => assert_ne!(s.editor.snap(t, &meter), t),
            _ => {}
        }
    }
    // A Grid-mode drag lands on a drawn (snap) line.
    let tracks = ArrangerView::lane_tracks(&s);
    let row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let clip = tracks[row].clips[0];
    let start = s.project().clip(clip).unwrap().start;
    view.scroll_x = start.quarters() * 300.0 - 200.0;
    let r = view.row_rect(row, SIZE);
    let grab = Point::new(view.x_of(start) + 30.0, r.y + r.h * 0.8);
    drag(
        &mut view,
        &mut s,
        grab,
        Point::new(grab.x + 137.0, grab.y),
        Modifiers::NONE,
    );
    let moved = s.project().clip(clip).unwrap().start;
    assert_ne!(moved, start);
    let drawn = view.grid_lines(
        moved - MusicalTime::QUARTER,
        moved + MusicalTime::QUARTER,
        &s,
    );
    assert!(
        drawn
            .iter()
            .any(|&(t, k)| t == moved && k != GridLineKind::Fine),
        "{moved:?} is on a drawn grid line"
    );
}

/// Zoomed in, the ruler names beats and finer lines; labels never crowd.
#[test]
fn the_ruler_labels_lines_when_zoomed_in() {
    let s = Session::demo(EngineConfig::default()).unwrap();
    let theme = Theme::default();
    let mut view = ArrangerView::new(theme.clone());
    for ppq in [300.0, 40_000.0, MAX_PPQ] {
        view.ppq = ppq;
        view.scroll_x = f64::from(ppq) * 1.3;
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &theme);
        let labels: Vec<f32> = p
            .ops
            .iter()
            .filter_map(|o| match o {
                DrawOp::Text(t, r) if t.starts_with("1.") && r.y < view.ruler_h() => Some(r.x),
                _ => None,
            })
            .collect();
        assert!(labels.len() >= 2, "{ppq}: {labels:?}");
        assert!(
            labels
                .windows(2)
                .all(|w| (w[1] - w[0]).abs() >= RULER_FINE_LABEL_PX - 0.5),
            "{ppq}: {labels:?}"
        );
    }
    let four =
        faderframe_timeline::TimeSignatureMap::new(faderframe_timeline::TimeSignature::FOUR_FOUR);
    let label = |q: f64, k| ruler_label(&four, MusicalTime::from_quarters(q), k);
    assert_eq!(label(1.25, GridLineKind::Subdivision), "1.2.240");
    assert_eq!(label(2.0, GridLineKind::Beat), "1.3");
    assert_eq!(label(1.0 + 1.0 / 512.0, GridLineKind::Fine), "1.2.001.875");
}

/// The Play Start Marker's flag drags in the ruler (one undo step, its
/// on/off kept), snapping like everything else; a loop edge under it
/// still drags from the edge's side, and a click on the flag changes
/// nothing (it used to toggle the loop).
#[test]
fn the_play_start_marker_drags_in_the_ruler_and_snaps() {
    let mut s = bare();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let (_, _, _, target) = bass_and_target(&s);
    let q = MusicalTime::from_quarters_i;
    let marker = faderframe_project::PlayStart {
        at: q(8),
        on: false,
    };
    s.dispatch(Action::Edit(Command::SetPlayStart {
        marker: Some(marker),
    }))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::SetLoop(
        MusicalRange::new(q(8), q(24)),
    )))
    .unwrap();
    let steps = s.history_steps().0.len();
    let y = LOOP_BAND * 0.5;
    // On the pennant: the marker, its tip 3 px from the target.
    let from = Point::new(view.x_of(q(8)) + 5.0, y);
    assert_eq!(view.hit_test(from, SIZE, &s), Some(Hit::PlayStart));
    let to = Point::new(view.x_of(target) + 5.0 + 3.0, y);
    let guide = drag(&mut view, &mut s, from, to, Modifiers::NONE);
    let moved = s.project().play_start.unwrap();
    assert_eq!(moved.at, target, "onto the clip edge");
    assert!(!moved.on, "still off");
    assert!(guide.is_some());
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    assert_eq!(
        s.project().loop_range,
        MusicalRange::new(q(8), q(24)),
        "the loop stays"
    );
    // Alt: free.
    let from = Point::new(view.x_of(target) + 5.0, y);
    let to = Point::new(view.x_of(target) + 5.0 + 3.0, y);
    drag(&mut view, &mut s, from, to, ALT);
    let px = (s.project().play_start.unwrap().at - target).quarters() * f64::from(view.ppq);
    assert!((px - 3.0).abs() < 0.5, "{px}");
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().play_start, Some(marker));
    // Left of the flag's line the loop's start edge is closer: it drags.
    let from = Point::new(view.x_of(q(8)) - 4.0, y);
    assert_eq!(view.hit_test(from, SIZE, &s), Some(Hit::LoopStart));
    // A click on the flag: nothing changes.
    let on_flag = Point::new(view.x_of(q(8)) + 6.0, y);
    let enabled = s.project().loop_enabled;
    drag(&mut view, &mut s, on_flag, on_flag, Modifiers::NONE);
    assert_eq!(s.project().loop_enabled, enabled);
    assert_eq!(s.project().play_start, Some(marker));
    // The marker is an object other drags snap to; its own drag skips it.
    let odd = q(8) + MusicalTime::from_ticks(12_345);
    s.dispatch(Action::Edit(Command::SetPlayStart {
        marker: Some(faderframe_project::PlayStart { at: odd, on: true }),
    }))
    .unwrap();
    let near = odd + MusicalTime::from_ticks(3);
    let reach = MusicalTime::from_ticks(10);
    let hit = s.snap_to_object(near, reach, &Default::default()).unwrap();
    assert_eq!((hit.object, hit.at), (SnapObject::PlayStart, odd));
    let skip = faderframe_session::SnapSkip::play_start_marker();
    assert_eq!(s.snap_to_object(near, reach, &skip), None);
}

/// A moved or added automation point snaps to an object near it (Alt:
/// free); tempo points and chords do too.
#[test]
fn automation_points_tempo_points_and_chords_snap_to_objects() {
    let mut s = bare();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let (row, _, start, target) = bass_and_target(&s);
    let bass = ArrangerView::lane_tracks(&s)[row].id;
    s.dispatch(Action::Edit(Command::AddAutomationLane {
        track: bass,
        lane: Box::new(faderframe_automation::AutomationLane {
            id: faderframe_core::AutomationLaneId(77_000),
            target: faderframe_automation::AutomationTarget::TrackVolume,
            curve: faderframe_automation::AutomationCurve::new(),
            mode: Default::default(),
            visible: true,
        }),
    }))
    .unwrap();
    if s.shown_lanes(bass).is_empty() {
        s.dispatch(Action::ToggleTrackAutomation(bass)).unwrap();
    }
    let lane_y = |view: &ArrangerView, s: &Session| {
        let t = s.project().track(bass).unwrap();
        let tracks = ArrangerView::lane_tracks(s);
        let i = tracks.iter().position(|x| x.id == bass).unwrap();
        let g = view.lane_geoms(s, t, view.row_rect(i, SIZE));
        g[0].value_area(view.header_w(), SIZE.w).center().y
    };
    let points = |s: &Session| -> Vec<MusicalTime> {
        s.project()
            .track(bass)
            .unwrap()
            .automation
            .lane(faderframe_automation::AutomationTarget::TrackVolume)
            .unwrap()
            .curve
            .points()
            .iter()
            .map(|p| p.time)
            .collect()
    };
    // A point added a quarter before the target and dragged to 4 px past
    // it lands on it.
    let y = lane_y(&view, &s);
    let from = Point::new(view.x_of(target - q(1)), y);
    let to = Point::new(view.x_of(target) + 4.0, y);
    let guide = drag(&mut view, &mut s, from, to, Modifiers::NONE);
    assert!(points(&s).contains(&target), "{:?}", points(&s));
    assert_eq!(guide.map(|g| g.at), Some(target));
    // Alt: where it was dropped.
    s.dispatch(Action::Undo).unwrap();
    drag(&mut view, &mut s, from, to, ALT);
    assert!(!points(&s).contains(&target));
    let _ = start;
    // A chord dragged by its body: its start onto the target.
    s.dispatch(Action::Edit(Command::SetChords {
        chords: vec![faderframe_project::ChordEvent {
            start: target - q(8),
            end: target - q(4),
            chord: faderframe_project::harmony::Chord::new(
                0,
                faderframe_project::harmony::Quality::Major,
            ),
        }],
    }))
    .unwrap();
    let lane = view
        .lane_rect(faderframe_session::lanes::GlobalLane::Chords, SIZE)
        .unwrap();
    let chord_start = s.project().chords[0].start;
    let from = Point::new(view.x_of(chord_start) + 20.0, lane.center().y);
    let to = Point::new(view.x_of(target) + 20.0 + 3.0, lane.center().y);
    drag(&mut view, &mut s, from, to, Modifiers::NONE);
    assert_eq!(s.project().chords[0].start, target);
    assert_eq!(s.project().chords[0].end, target + q(4));
    // A tempo point dragged sideways to 3 px past the target.
    s.dispatch(Action::AddTempoPoint(target - q(4))).unwrap();
    let points = s.project().timeline.tempo.points().to_vec();
    let index = points
        .iter()
        .position(|p| p.position == target - q(4))
        .unwrap();
    let lane = view
        .lane_rect(faderframe_session::lanes::GlobalLane::Tempo, SIZE)
        .unwrap();
    let y = ArrangerView::tempo_y_for_test(lane, &s, points[index].bpm);
    let from = Point::new(view.x_of(target - q(4)), y);
    assert_eq!(
        view.hit_test(from, SIZE, &s),
        Some(Hit::Global(GlobalHit::Tempo(index)))
    );
    let to = Point::new(view.x_of(target) + 3.0, y);
    drag(&mut view, &mut s, from, to, Modifiers::NONE);
    let moved = s.project().timeline.tempo.points().to_vec();
    assert!(
        moved.iter().any(|p| p.position == target),
        "{moved:?} has a point at {target:?}"
    );
}

fn q(n: i64) -> MusicalTime {
    MusicalTime::from_quarters_i(n)
}

const ALT: Modifiers = Modifiers {
    alt: true,
    ..Modifiers::NONE
};
