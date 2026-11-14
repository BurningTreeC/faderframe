//! Clip envelopes and the Strip Silence preview in the arranger: with a
//! lane shown, a click on an audio clip's body adds a point and drags it
//! (one undo step), a double-click removes it, a point's menu sets its
//! shape; the clip menu shows, hides and clears lanes and opens Strip
//! Silence; the dialog's regions are drawn on the clips.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_project::clip_envelope::{EnvelopeLane, EnvelopePoint};
use faderframe_session::ClipEdge;
use faderframe_ui_canvas::{DrawOp, RecordingPainter};

fn send(view: &mut ArrangerView, s: &mut Session, ev: ViewEvent, size: Size) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    {
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, size, s, &mut cx);
    }
    for a in &actions {
        s.dispatch(a.clone()).unwrap();
    }
    actions
}

fn press(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

fn moved(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn released(pos: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

/// y of an envelope value in a clip's content area.
fn view_y(content: Rect, v: f64) -> f32 {
    content.bottom() - EnvelopeLane::Volume.to_unit(v) as f32 * content.h
}

fn hover(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: false,
    }
}

fn bass(s: &Session) -> ClipId {
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap();
    t.clips[0]
}

fn volume(s: &Session, clip: ClipId) -> Vec<EnvelopePoint> {
    s.clip_envelope(clip.into())
        .map(|e| e.volume.clone())
        .unwrap_or_default()
}

#[test]
fn a_click_adds_a_point_a_drag_moves_it_and_a_double_click_removes_it() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let clip = bass(&s);
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.content_rect(rect);
    let c = s.project().clip(clip).unwrap().clone();
    let at = Point::new(rect.x + rect.w * 0.4, content.y + content.h * 0.5);
    // Hidden: the body moves the clip as ever.
    assert_ne!(view.clip_zone(&s, &c, rect, at), ClipZone::Envelope);
    s.dispatch(Action::ShowClipEnvelope(Some(EnvelopeLane::Volume)))
        .unwrap();
    assert_eq!(view.clip_zone(&s, &c, rect, at), ClipZone::Envelope);
    // The edges still trim.
    let edge = Point::new(rect.right() - 2.0, at.y);
    assert_eq!(
        view.clip_zone(&s, &c, rect, edge),
        ClipZone::Trim(ClipEdge::End)
    );
    let steps = s.history_steps().0.len();
    // Pressed below the line (0 dB, three quarters up): the point goes on
    // the line, not where the click landed — adding never moves it.
    send(&mut view, &mut s, press(at, 1), size);
    let added = volume(&s, clip);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].value, 0.0, "{added:?}");
    // Then dragged down by a quarter of the clip: half way up the scale.
    let down = Point::new(at.x, at.y + content.h * 0.25);
    send(&mut view, &mut s, moved(Point::new(at.x, at.y + 5.0)), size);
    send(&mut view, &mut s, moved(down), size);
    send(&mut view, &mut s, released(down), size);
    let dragged = volume(&s, clip);
    assert_eq!(dragged.len(), 1);
    assert_eq!(
        dragged[0].at, added[0].at,
        "Shift-free drags keep it on the grid"
    );
    let half = EnvelopeLane::Volume.from_unit(0.5);
    assert!(
        (dragged[0].value - half).abs() < 0.3,
        "{dragged:?} vs {half}"
    );
    let zero = Point::new(at.x, view_y(content, dragged[0].value));
    // One step for the click and the drag.
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // Painted: the line and the point.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let px = view.x_of_frame(&s, &c, dragged[0].at);
    assert!(p.ops.iter().any(|op| matches!(op,
        DrawOp::Rect(r) if (r.center().x - px).abs() < 0.5 && (r.w - 6.0).abs() < 0.01)));
    let _ = zero;
    // The point's menu sets its shape.
    let point = Point::new(px, zero.y);
    let Some(HostRequest::ContextMenu { items, .. }) =
        view.envelope_point_menu(&s, size, &c, point)
    else {
        panic!("a point menu");
    };
    let step = items
        .iter()
        .find(|i| i.label.starts_with("Step"))
        .and_then(|i| i.action.clone())
        .unwrap();
    s.dispatch(step).unwrap();
    assert_eq!(
        volume(&s, clip)[0].shape,
        faderframe_automation::CurveShape::Step
    );
    // A double-click on it removes it.
    send(&mut view, &mut s, press(point, 2), size);
    assert!(volume(&s, clip).is_empty());
}

#[test]
fn the_clip_menu_shows_clears_envelopes_and_opens_strip_silence() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let clip = bass(&s);
    s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Pan,
        points: vec![EnvelopePoint::new(0, 0.5)],
    })
    .unwrap();
    let c = s.project().clip(clip).unwrap().clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::clip_menu(&s, &c, Point::new(0.0, 0.0))
    else {
        panic!("a menu")
    };
    let env = items.iter().find(|i| i.label == "Clip Envelope").unwrap();
    let find = |label: &str| env.children.iter().find(|i| i.label == label).unwrap();
    assert_eq!(find("Hide").checked, Some(true));
    assert!(
        find("Clear Volume Envelope").action.is_none(),
        "no volume points"
    );
    s.dispatch(find("Clear Pan Envelope").action.clone().unwrap())
        .unwrap();
    assert!(s.clip_envelope(clip.into()).is_none());
    s.dispatch(find("Show Pan").action.clone().unwrap())
        .unwrap();
    assert_eq!(s.clip_envelope_shown(), Some(EnvelopeLane::Pan));
    let strip = items
        .iter()
        .find(|i| i.label == "Strip Silence…")
        .and_then(|i| i.action.clone())
        .unwrap();
    assert!(matches!(
        strip,
        Action::StripSilence(faderframe_session::strip_silence::StripSilenceOp::Open { .. })
    ));
}

#[test]
fn the_strip_silence_preview_shades_the_silences() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    let clip = bass(&s);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let before = p.ops.len();
    s.dispatch(Action::StripSilence(
        faderframe_session::strip_silence::StripSilenceOp::Open { clips: vec![clip] },
    ))
    .unwrap();
    assert!(s.wait_for_strip_silence(std::time::Duration::from_secs(30)));
    let preview = s.strip_silence_preview().unwrap();
    assert!(!preview.regions[&clip].is_empty());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.ops.len() > before, "regions drawn");
}

/// A point between two others goes on the line between them (its value
/// interpolated), so the line keeps its shape; the point under the
/// pointer is its own zone (a move cursor, lit, its value beside it), the
/// line elsewhere adds (a plus); a point grabbed off its centre moves by
/// how far the pointer goes, not to it.
#[test]
fn adding_keeps_the_line_and_the_point_under_the_pointer_is_lit() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let clip = bass(&s);
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.content_rect(rect);
    let c = s.project().clip(clip).unwrap().clone();
    let len = c.as_audio().unwrap().length;
    s.dispatch(Action::ShowClipEnvelope(Some(EnvelopeLane::Volume)))
        .unwrap();
    s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Volume,
        points: vec![EnvelopePoint::new(0, -12.0), EnvelopePoint::new(len, 0.0)],
    })
    .unwrap();
    // A click half way, well off the line, with snapping off (Alt).
    let x = rect.x + rect.w * 0.5;
    let mut ev = press(Point::new(x, content.y + 4.0), 1);
    if let ViewEvent::PointerDown { modifiers, .. } = &mut ev {
        modifiers.alt = true;
    }
    send(&mut view, &mut s, ev, size);
    send(
        &mut view,
        &mut s,
        released(Point::new(x, content.y + 4.0)),
        size,
    );
    let points = volume(&s, clip);
    assert_eq!(points.len(), 3);
    let mid = points[1];
    let on_line = -12.0 + 12.0 * mid.at as f64 / len as f64;
    assert!((mid.value - on_line).abs() < 1e-6, "{points:?}");
    // Hovering it: its zone, a move cursor, drawn lit with its value.
    let px = view.x_of_frame(&s, &c, mid.at);
    let on = Point::new(px + 3.0, view_y(content, mid.value) + 2.0);
    let zone = view.clip_zone(&s, &c, rect, on);
    assert_eq!(zone, ClipZone::EnvelopePoint(1));
    assert_eq!(ArrangerView::zone_cursor(zone), Cursor::Move);
    send(&mut view, &mut s, hover(on), size);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.ops.iter().any(|op| matches!(op,
        DrawOp::Rect(r) if (r.center().x - px).abs() < 0.5 && (r.w - 9.0).abs() < 0.01)));
    let label = EnvelopeLane::Volume.value_text(mid.value);
    assert!(p.texts().contains(&label.as_str()), "{label}");
    let tip = view.envelope_tooltip(&s, size, on).unwrap();
    assert!(tip.starts_with(&format!("Volume {label} at ")), "{tip}");
    // Elsewhere on the clip, off the line: adding, a plus.
    let off = Point::new(rect.x + rect.w * 0.25, content.bottom() - 2.0);
    let zone = view.clip_zone(&s, &c, rect, off);
    assert_eq!(zone, ClipZone::Envelope);
    assert_eq!(ArrangerView::zone_cursor(zone), Cursor::Add);
    // Grabbed 3 px off its centre and dragged 20 px up: it rises by 20 px
    // on the scale, not to the pointer.
    send(&mut view, &mut s, press(on, 1), size);
    let up = Point::new(on.x, on.y - 20.0);
    send(&mut view, &mut s, moved(Point::new(on.x, on.y - 5.0)), size);
    send(&mut view, &mut s, moved(up), size);
    send(&mut view, &mut s, released(up), size);
    let moved_to = volume(&s, clip)[1].value;
    let expected = EnvelopeLane::Volume
        .from_unit(EnvelopeLane::Volume.to_unit(mid.value) + 20.0 / f64::from(content.h));
    assert!(
        (moved_to - expected).abs() < 0.05,
        "{moved_to} vs {expected}"
    );
    assert_eq!(volume(&s, clip).len(), 3, "moved, not added");
}

/// The first and last points sit on the clip's edges: over them the point
/// wins (a move, not a trim); a little above or below, the edge still
/// trims.
#[test]
fn points_on_the_clips_edges_win_over_trimming() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let clip = bass(&s);
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.content_rect(rect);
    let c = s.project().clip(clip).unwrap().clone();
    let len = c.as_audio().unwrap().length;
    s.dispatch(Action::ShowClipEnvelope(Some(EnvelopeLane::Volume)))
        .unwrap();
    s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Volume,
        points: vec![EnvelopePoint::new(0, -6.0), EnvelopePoint::new(len, -6.0)],
    })
    .unwrap();
    let y = view_y(content, -6.0);
    for (i, x, edge) in [
        (0, rect.x + 1.0, ClipEdge::Start),
        (1, rect.right() - 1.0, ClipEdge::End),
    ] {
        assert_eq!(
            view.clip_zone(&s, &c, rect, Point::new(x, y)),
            ClipZone::EnvelopePoint(i)
        );
        assert_eq!(
            view.clip_zone(&s, &c, rect, Point::new(x, y + 12.0)),
            ClipZone::Trim(edge),
            "away from the point the edge trims"
        );
    }
}

/// The line between two points is grabbed: a drag moves both points up or
/// down together (their times stay; one undo step), a click without a drag
/// adds a point there; hovering it lights both points and the pointer
/// says up or down.
#[test]
fn the_line_between_two_points_is_dragged_and_clicked() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let size = Size::new(1400.0, 700.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let clip = bass(&s);
    let rect = view.clip_view_rect(&s, size, clip).unwrap();
    let content = view.content_rect(rect);
    let c = s.project().clip(clip).unwrap().clone();
    let len = c.as_audio().unwrap().length;
    s.dispatch(Action::ShowClipEnvelope(Some(EnvelopeLane::Volume)))
        .unwrap();
    let (a, b) = (len / 4, len * 3 / 4);
    s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Volume,
        points: vec![EnvelopePoint::new(a, -6.0), EnvelopePoint::new(b, -6.0)],
    })
    .unwrap();
    let on = Point::new(
        view.x_of_frame(&s, &c, (a + b) / 2),
        view_y(content, -6.0) + 2.0,
    );
    let zone = view.clip_zone(&s, &c, rect, on);
    assert_eq!(zone, ClipZone::EnvelopeSegment);
    assert_eq!(ArrangerView::zone_cursor(zone), Cursor::ResizeVertical);
    // Hovered: both points lit.
    send(&mut view, &mut s, hover(on), size);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let lit = p
        .ops
        .iter()
        .filter(|op| matches!(op, DrawOp::Rect(r) if (r.w - 9.0).abs() < 0.01))
        .count();
    assert_eq!(lit, 2, "both points lit");
    // Dragged 20 px up: both rise alike, their times kept, one step.
    let steps = s.history_steps().0.len();
    send(&mut view, &mut s, press(on, 1), size);
    let up = Point::new(on.x + 30.0, on.y - 20.0);
    send(&mut view, &mut s, moved(Point::new(on.x, on.y - 5.0)), size);
    send(&mut view, &mut s, moved(up), size);
    send(&mut view, &mut s, released(up), size);
    let after = volume(&s, clip);
    assert_eq!(after.len(), 2, "no point added");
    assert_eq!((after[0].at, after[1].at), (a, b));
    let expected = EnvelopeLane::Volume
        .from_unit(EnvelopeLane::Volume.to_unit(-6.0) + 20.0 / f64::from(content.h));
    for p in &after {
        assert!((p.value - expected).abs() < 0.05, "{after:?} vs {expected}");
    }
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // A click on it without a drag: a point there, on the line.
    let on = Point::new(on.x, view_y(content, after[0].value) + 1.0);
    send(&mut view, &mut s, press(on, 1), size);
    send(&mut view, &mut s, released(on), size);
    let added = volume(&s, clip);
    assert_eq!(added.len(), 3);
    assert!((added[1].value - after[0].value).abs() < 1e-6, "{added:?}");
}
