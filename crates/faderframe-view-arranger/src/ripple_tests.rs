//! Ripple editing in the arranger: the delete keys, leaving a track out,
//! the corner's status and what a ripple drag shows.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::{EditMode, EditRange, RippleOp, RippleScope, RippleSettings};
use faderframe_ui_canvas::RecordingPainter;

const SIZE: Size = Size::new(1400.0, 700.0);

fn run(view: &mut ArrangerView, ev: ViewEvent, s: &Session) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    actions
}

fn painted(view: &mut ArrangerView, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn shift(ctrl: bool) -> Modifiers {
    Modifiers {
        shift: true,
        ctrl,
        ..Modifiers::NONE
    }
}

/// Shift+Delete ripple-deletes the range, else the selected clips, as far
/// as the settings reach; Ctrl+Shift+Delete in all tracks.
#[test]
fn shift_delete_ripples_and_ctrl_reaches_every_track() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let bass = s.project().track(track(&s, "Bass")).unwrap().clips[0];
    s.dispatch(Action::SelectClips {
        clips: vec![bass],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(None)).unwrap();
    let key = |view: &mut ArrangerView, s: &Session, m| {
        run(
            view,
            ViewEvent::Key {
                key: Key::Delete,
                modifiers: m,
            },
            s,
        )
    };
    assert_eq!(
        key(&mut view, &s, shift(false)),
        vec![Action::Ripple(RippleOp::DeleteClips(RippleScope::Edited))]
    );
    assert_eq!(
        key(&mut view, &s, shift(true)),
        vec![Action::Ripple(RippleOp::DeleteClips(RippleScope::All))]
    );
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        MusicalTime::from_quarters_i(4),
        MusicalTime::from_quarters_i(8),
    ))))
    .unwrap();
    assert_eq!(
        key(&mut view, &s, shift(false)),
        vec![Action::Ripple(RippleOp::DeleteRange(RippleScope::Edited))]
    );
    // Plain Delete as ever.
    assert_eq!(
        key(&mut view, &s, Modifiers::NONE),
        vec![Action::DeleteInArranger]
    );
}

/// The track menu leaves a track out of ripples; its header says so.
#[test]
fn the_track_menu_leaves_a_track_out() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let bass = track(&s, "Bass");
    let t = s.project().track(bass).unwrap().clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::track_menu(&s, &t, Point::new(0.0, 0.0))
    else {
        panic!("the track menu")
    };
    let item = items
        .iter()
        .find(|i| i.label == "Leave Out of Ripples")
        .unwrap();
    assert_eq!(item.checked, Some(false));
    s.dispatch(item.action.clone().unwrap()).unwrap();
    assert!(s.project().track(bass).unwrap().ripple_excluded);
    let p = painted(&mut view, &s);
    assert!(
        p.texts().iter().any(|t| t.starts_with("No ripple · Audio")),
        "{:?}",
        p.texts()
    );
}

/// The corner names where ripples reach while editing ripples.
#[test]
fn the_corner_shows_the_ripple() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let corner = |view: &mut ArrangerView, s: &Session| {
        painted(view, s)
            .texts()
            .iter()
            .any(|t| t.contains("RIPPLE ALL"))
    };
    assert!(!corner(&mut view, &s));
    let r = s.ripple_settings();
    s.dispatch(Action::Ripple(RippleOp::Settings(RippleSettings {
        on: true,
        scope: RippleScope::All,
        ..r
    })))
    .unwrap();
    assert!(corner(&mut view, &s));
}

/// While a ripple trim drags, the arranger knows where its edit is and
/// which tracks move (all of them, with All Tracks); Shuffle reorders
/// show their ghost instead.
#[test]
fn a_ripple_trim_shows_what_moves() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    let r = s.ripple_settings();
    s.dispatch(Action::Ripple(RippleOp::Settings(RippleSettings {
        on: true,
        scope: RippleScope::All,
        ..r
    })))
    .unwrap();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let bass = track(&s, "Bass");
    let clip = s.project().clips_of(bass)[0].id;
    let rect = view.clip_view_rect(&s, SIZE, clip).unwrap();
    // The end edge, in the lower half (the Smart tool trims there).
    let from = Point::new(rect.right() - 2.0, rect.y + rect.h * 0.75);
    for a in run(
        &mut view,
        ViewEvent::PointerDown {
            pos: from,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &s,
    ) {
        s.dispatch(a).unwrap();
    }
    for dx in [-20.0, -40.0] {
        for a in run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(from.x + dx, from.y),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            &s,
        ) {
            s.dispatch(a).unwrap();
        }
    }
    let (cut, edited) = view.ripple_drag(&s).expect("a ripple drag");
    let p = s.project();
    let c = p.clip(clip).unwrap();
    assert_eq!(cut, c.end(&p.timeline, p.sample_rate), "at the trimmed end");
    assert_eq!(edited, vec![bass]);
    assert!(
        s.ripple_followers(&edited, RippleScope::All).len() > 2,
        "the other tracks follow"
    );
    painted(&mut view, &s);
    for a in run(
        &mut view,
        ViewEvent::PointerUp {
            pos: Point::new(from.x - 40.0, from.y),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        &s,
    ) {
        s.dispatch(a).unwrap();
    }
    assert!(view.ripple_drag(&s).is_none(), "done");
}
