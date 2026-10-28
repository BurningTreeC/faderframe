//! Time-locked tracks in the arranger: the track menu's Time Base, the
//! padlock on the header and the mark on its clips.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_project::TimeBase;
use faderframe_ui_canvas::{DrawOp, RecordingPainter};

const SIZE: Size = Size::new(1400.0, 700.0);

fn painted(view: &mut ArrangerView, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p
}

fn bass(s: &Session) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id
}

/// The Time Base submenu: (label, checked) of its entries.
fn time_base_menu(s: &Session, t: TrackId) -> Vec<MenuItem<Action>> {
    let track = s.project().track(t).unwrap().clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::track_menu(s, &track, Point::new(0.0, 0.0))
    else {
        panic!("the track menu")
    };
    items
        .into_iter()
        .find(|i| i.label == "Time Base")
        .map(|i| i.children)
        .unwrap()
}

#[test]
fn the_track_menu_chooses_the_time_base() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let t = bass(&s);
    let menu = time_base_menu(&s, t);
    let shown = |m: &[MenuItem<Action>]| {
        m.iter()
            .map(|i| (i.label.clone(), i.checked))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shown(&menu),
        vec![
            ("Musical".to_string(), Some(true)),
            ("Time-locked".to_string(), Some(false)),
        ]
    );
    s.dispatch(menu[1].action.clone().unwrap()).unwrap();
    assert_eq!(
        s.project().track(t).unwrap().time_base,
        TimeBase::TimeLocked
    );
    assert_eq!(
        shown(&time_base_menu(&s, t)),
        vec![
            ("Musical".to_string(), Some(false)),
            ("Time-locked".to_string(), Some(true)),
        ]
    );
}

#[test]
fn a_time_locked_track_shows_a_padlock_and_marks_its_clips() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    let t = bass(&s);
    painted(&mut view, &s);
    let l = view.header_layout(&s, t, SIZE).unwrap();
    let (glyph, rest) = header::lock_rect(l.name);
    let in_glyph = |p: &RecordingPainter| {
        p.ops
            .iter()
            .any(|o| matches!(o, DrawOp::Rounded(r) if glyph.contains(r.center())))
    };
    let name_at = |p: &RecordingPainter| {
        p.ops.iter().find_map(|o| match o {
            DrawOp::Text(text, r) if text == "Bass" => Some(r.x),
            _ => None,
        })
    };
    let clip = s.project().clips_of(t)[0].name.clone();
    let clip_name_at = |p: &RecordingPainter| {
        p.ops.iter().find_map(|o| match o {
            DrawOp::Text(text, r) if *text == clip => Some(r.x),
            _ => None,
        })
    };
    let before = painted(&mut view, &s);
    assert!(!in_glyph(&before), "a musical track has none");
    assert_eq!(name_at(&before), Some(l.name.x));
    let clip_x = clip_name_at(&before).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackTimeBase {
        track: t,
        base: TimeBase::TimeLocked,
    }))
    .unwrap();
    let after = painted(&mut view, &s);
    assert!(in_glyph(&after), "the padlock before the name");
    assert_eq!(name_at(&after), Some(rest.x), "the name after it");
    assert_eq!(
        clip_name_at(&after),
        Some(clip_x + 10.0),
        "its clips' names after a small padlock"
    );
    // Other tracks show none.
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    let dl = view.header_layout(&s, drums, SIZE).unwrap();
    let (other, _) = header::lock_rect(dl.name);
    assert!(
        !after
            .ops
            .iter()
            .any(|o| matches!(o, DrawOp::Rounded(r) if other.contains(r.center())))
    );
}
