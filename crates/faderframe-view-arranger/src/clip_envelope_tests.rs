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
    s.clip_envelope(clip)
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
    // Pressed below 0 dB (three quarters up): a point there, then dragged
    // up to 0 dB.
    send(&mut view, &mut s, press(at, 1), size);
    let added = volume(&s, clip);
    assert_eq!(added.len(), 1);
    assert!(added[0].value < -3.0, "{added:?}");
    let zero = Point::new(at.x, content.bottom() - 0.75 * content.h);
    send(&mut view, &mut s, moved(Point::new(at.x, at.y - 5.0)), size);
    send(&mut view, &mut s, moved(zero), size);
    send(&mut view, &mut s, released(zero), size);
    let dragged = volume(&s, clip);
    assert_eq!(dragged.len(), 1);
    assert_eq!(
        dragged[0].at, added[0].at,
        "Shift-free drags keep it on the grid"
    );
    assert!(dragged[0].value.abs() < 0.3, "{dragged:?}");
    // One step for the click and the drag.
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // Painted: the line and the point.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let px = view.x_of_frame(&s, &c, dragged[0].at);
    assert!(p.ops.iter().any(|op| matches!(op,
        DrawOp::Rect(r) if (r.center().x - px).abs() < 0.5 && (r.w - 6.0).abs() < 0.01)));
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
        clip,
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
    assert!(s.clip_envelope(clip).is_none());
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
