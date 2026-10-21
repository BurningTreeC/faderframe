//! The arranger audit (§2.9): selection gestures and the edit range,
//! Delete, the Separation Grabber, the nudge keys.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::{EditMode, EditRange, EditTool, NudgeTarget};
use faderframe_ui_canvas::RecordingPainter;

const SIZE: Size = Size::new(1400.0, 700.0);

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn run(
    view: &mut ArrangerView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

/// Press at `from` with `m`, move through `path`, release; every action
/// dispatched as it comes.
fn drag(view: &mut ArrangerView, s: &mut Session, from: Point, path: &[Point], m: Modifiers) {
    let press = ViewEvent::PointerDown {
        pos: from,
        button: PointerButton::Primary,
        modifiers: m,
        clicks: 1,
    };
    for a in run(view, press, s).0 {
        s.dispatch(a).unwrap();
    }
    let mut last = from;
    for p in path {
        let ev = ViewEvent::PointerMove {
            pos: *p,
            modifiers: m,
            dragging: true,
        };
        for a in run(view, ev, s).0 {
            s.dispatch(a).unwrap();
        }
        last = *p;
    }
    let ev = ViewEvent::PointerUp {
        pos: last,
        button: PointerButton::Primary,
        modifiers: m,
    };
    for a in run(view, ev, s).0 {
        s.dispatch(a).unwrap();
    }
}

fn key(view: &mut ArrangerView, s: &Session, key: Key, modifiers: Modifiers) -> Vec<Action> {
    run(view, ViewEvent::Key { key, modifiers }, s).0
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn first_clip(s: &Session, name: &str) -> ClipId {
    s.project().track(track(s, name)).unwrap().clips[0]
}

fn painted(view: &mut ArrangerView, s: &Session) {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
}

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    ..Modifiers::NONE
};
const SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::NONE
};

/// A range drawn earlier, then clips selected by Ctrl-click, Shift-click
/// or a rectangle: the clips are what Delete, Copy, Cut and Duplicate work
/// on now — the old range goes (it used to win, clearing a time span on
/// the clips' tracks instead).
#[test]
fn selecting_clips_drops_a_stale_edit_range() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let (bass, pluck) = (first_clip(&s, "Bass"), first_clip(&s, "Pluck"));
    let lower = |view: &ArrangerView, s: &Session, c: ClipId| {
        let r = view.clip_view_rect(s, SIZE, c).unwrap();
        Point::new(r.x + 60.0, r.y + r.h * 0.8)
    };
    let range = |s: &mut Session| {
        s.dispatch(Action::SetEditRange(Some(EditRange::new(
            MusicalTime::from_quarters_i(1),
            MusicalTime::from_quarters_i(3),
        ))))
        .unwrap();
        assert!(s.selection.range.is_some());
    };
    // Ctrl-click.
    range(&mut s);
    let at = lower(&view, &s, bass);
    drag(&mut view, &mut s, at, &[], CTRL);
    assert!(s.selection.clips.contains(&bass));
    assert_eq!(s.selection.range, None, "Ctrl-click");
    // Shift-click from the anchor to another track's clip.
    range(&mut s);
    let at = lower(&view, &s, pluck);
    drag(&mut view, &mut s, at, &[], SHIFT);
    assert!(s.selection.clips.len() >= 2);
    assert_eq!(s.selection.range, None, "Shift-click");
    // A rectangle (Ctrl+drag) over the Bass clip.
    range(&mut s);
    let from = lower(&view, &s, bass);
    drag(
        &mut view,
        &mut s,
        from,
        &[
            Point::new(from.x + 30.0, from.y + 4.0),
            Point::new(from.x + 60.0, from.y + 6.0),
        ],
        CTRL,
    );
    assert_eq!(s.selection.range, None, "the rectangle");
    // Delete now deletes the selected clips.
    let selected = s.selected_arranger_clips();
    assert!(!selected.is_empty());
    let acts = key(&mut view, &s, Key::Delete, Modifiers::NONE);
    assert_eq!(acts, vec![Action::DeleteInArranger]);
    for a in acts {
        s.dispatch(a).unwrap();
    }
    for c in selected {
        assert!(s.project().clip(c).is_none(), "{c:?} deleted");
    }
}

/// The clip menu's Delete deletes those clips, also with an edit range
/// (which Delete would clear instead).
#[test]
fn the_clip_menus_delete_deletes_its_clips_with_a_range_too() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let bass = first_clip(&s, "Bass");
    let r = view.clip_view_rect(&s, SIZE, bass).unwrap();
    s.dispatch(Action::SelectClips {
        clips: vec![bass],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        MusicalTime::from_quarters_i(40),
        MusicalTime::from_quarters_i(44),
    ))))
    .unwrap();
    let ev = ViewEvent::PointerDown {
        pos: Point::new(r.x + 40.0, r.y + r.h * 0.8),
        button: PointerButton::Secondary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    };
    let (_, req) = run(&mut view, ev, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("no menu");
    };
    let delete = items
        .iter()
        .find(|i| i.label == "Delete")
        .and_then(|i| i.action.clone())
        .unwrap();
    let range = s.selection.range;
    s.dispatch(delete).unwrap();
    assert!(s.project().clip(bass).is_none());
    assert_eq!(s.selection.range, range, "the range is not what went");
}

/// The Separation Grabber: a click inside the range changes nothing; a
/// drag takes the range out of every selected track's clips and moves the
/// pieces together — one undo step.
#[test]
fn the_separation_grabber_moves_the_range_of_every_track_in_one_step() {
    let mut s = session();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    s.dispatch(Action::SetEditTool(EditTool::GrabSeparation))
        .unwrap();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let (bass, pluck) = (track(&s, "Bass"), track(&s, "Pluck"));
    let p = s.project();
    let clip_span = |t: TrackId| {
        let c = p.clips_of(t)[0];
        (c.start, c.end(&p.timeline, p.sample_rate))
    };
    let (b0, b1) = clip_span(bass);
    let (p0, p1) = clip_span(pluck);
    let (a, b) = (b0.max(p0), b1.min(p1));
    assert!(b > a + MusicalTime::from_quarters_i(4), "the clips overlap");
    let r = EditRange::new(
        a + MusicalTime::QUARTER,
        a + MusicalTime::from_quarters_i(3),
    );
    s.dispatch(Action::SelectTracks {
        tracks: vec![bass, pluck],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(r))).unwrap();
    let count = |s: &Session| {
        (
            s.project().clips_of(bass).len(),
            s.project().clips_of(pluck).len(),
        )
    };
    let before = count(&s);
    let steps = s.history_steps().0.len();
    let bass_clip = s.project().clips_of(bass)[0].id;
    let rect = view.clip_view_rect(&s, SIZE, bass_clip).unwrap();
    let at = Point::new(
        view.x_of(r.start + MusicalTime::QUARTER),
        rect.y + rect.h * 0.8,
    );
    // A click: nothing.
    drag(&mut view, &mut s, at, &[], Modifiers::NONE);
    assert_eq!(count(&s), before);
    assert_eq!(s.history_steps().0.len(), steps);
    // A drag two quarters later.
    let to = Point::new(at.x + 2.0 * view.ppq, at.y);
    drag(
        &mut view,
        &mut s,
        at,
        &[
            Point::new(at.x + 6.0, at.y),
            Point::new(at.x + 12.0, at.y),
            to,
        ],
        Modifiers::NONE,
    );
    assert_eq!(
        count(&s),
        (before.0 + 2, before.1 + 2),
        "both tracks separated"
    );
    let moved = |s: &Session, t: TrackId| {
        s.project()
            .clips_of(t)
            .iter()
            .any(|c| c.start == r.start + MusicalTime::from_quarters_i(2))
    };
    assert!(moved(&s, bass) && moved(&s, pluck), "both pieces moved");
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(count(&s), before);
}

/// Ctrl+, opens the Preferences (an application shortcut): Ctrl+Alt+,
/// and Ctrl+Alt+. nudge the clips' ends as Ctrl+. does.
#[test]
fn ctrl_alt_nudges_trim_the_end() {
    let s = session();
    let mut view = ArrangerView::new(Theme::default());
    let ctrl_alt = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::NONE
    };
    for (k, forward) in [(',', false), ('.', true)] {
        assert_eq!(
            key(&mut view, &s, Key::Char(k), ctrl_alt),
            vec![Action::Nudge {
                forward,
                target: NudgeTarget::TrimEnd
            }]
        );
    }
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    assert_eq!(
        key(&mut view, &s, Key::Char(','), alt),
        vec![Action::Nudge {
            forward: false,
            target: NudgeTarget::TrimStart
        }]
    );
}

/// A double-click on an instrument lane makes a MIDI clip with the editing
/// tools, not with the Zoom or Scrub tool (two zooms, two scrubs).
#[test]
fn zoom_and_scrub_double_clicks_make_no_midi_clip() {
    for (tool, makes) in [
        (EditTool::Smart, true),
        (EditTool::Zoom, false),
        (EditTool::Scrub, false),
    ] {
        let mut s = session();
        s.dispatch(Action::SetEditTool(tool)).unwrap();
        let mut view = ArrangerView::new(Theme::default());
        let tracks = ArrangerView::lane_tracks(&s);
        let row_i = tracks
            .iter()
            .position(|t| t.kind == TrackKind::Instrument)
            .unwrap();
        let row = view.row_rect(row_i, SIZE);
        let x = view.x_of(MusicalTime::from_quarters(1.5));
        let ev = ViewEvent::PointerDown {
            pos: Point::new(x, row.center().y),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 2,
        };
        let (a, _) = run(&mut view, ev, &s);
        let made = a.iter().any(|a| matches!(a, Action::CreateMidiClip { .. }));
        assert_eq!(made, makes, "{tool:?}");
    }
}

/// Shift-click in a lane extends the range in time and over the rows
/// between the selected tracks and the clicked one (it used to add only
/// the clicked track, leaving the tracks between out of the range edit).
#[test]
fn shift_click_in_a_lane_extends_the_range_over_the_rows_between() {
    let mut s = session();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let rows = ArrangerView::lane_tracks(&s);
    let ids: Vec<TrackId> = rows.iter().map(|t| t.id).collect();
    let pad_row = rows.iter().position(|t| t.name == "Pad").unwrap();
    let drums = track(&s, "Drums");
    s.dispatch(Action::SelectTracks {
        tracks: vec![drums],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        MusicalTime::from_quarters_i(2),
        MusicalTime::from_quarters_i(4),
    ))))
    .unwrap();
    // The Pad lane is empty before its clip (bar 5).
    let r = view.row_rect(pad_row, SIZE);
    let at = Point::new(view.x_of(MusicalTime::from_quarters_i(7)), r.center().y);
    assert!(matches!(
        view.hit_test(at, SIZE, &s),
        Some(Hit::Lane { .. })
    ));
    drag(&mut view, &mut s, at, &[], SHIFT);
    let r = s.selection.range.unwrap();
    assert_eq!(
        (r.start, r.end),
        (
            MusicalTime::from_quarters_i(2),
            MusicalTime::from_quarters_i(7)
        )
    );
    let drums_row = ids.iter().position(|t| *t == drums).unwrap();
    let want: Vec<TrackId> = ids[drums_row..=pad_row].to_vec();
    let mut got: Vec<TrackId> = s.selection.tracks.iter().copied().collect();
    got.sort_by_key(|t| ids.iter().position(|x| x == t));
    assert_eq!(got, want);
}
