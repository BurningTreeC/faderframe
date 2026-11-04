//! Time-locked tracks in the arranger: the track menu's Time Base, the
//! padlock on the header and the mark on its clips; markers and sections
//! locked to time from their menus.

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

/// A marker and a section locked to time from their menus: checked, a
/// padlock before the name, said in the tooltip; tempo changes then keep
/// them where they are.
#[test]
fn markers_and_sections_lock_to_time_from_their_menus() {
    use crate::global::{GlobalHit, SectionPart};
    use faderframe_session::lanes::GlobalLane;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let mut view = ArrangerView::new(Theme::default());
    painted(&mut view, &s);
    let marker = s.project().markers[0].clone();
    let lock_item = |items: Vec<MenuItem<Action>>| {
        items
            .into_iter()
            .find(|i| i.label == "Lock to Time")
            .expect("Lock to Time")
    };
    let HostRequest::ContextMenu { items, .. } =
        view.global_menu(GlobalHit::Marker(marker.id), &s, Point::new(0.0, 0.0))
    else {
        panic!("the marker menu")
    };
    let lock = lock_item(items);
    assert_eq!(lock.checked, Some(false));
    s.dispatch(lock.action.unwrap()).unwrap();
    let m = s
        .project()
        .markers
        .iter()
        .find(|m| m.id == marker.id)
        .unwrap()
        .clone();
    assert!(m.is_time_locked());
    let lane = view.lane_rect(GlobalLane::Markers, SIZE).unwrap();
    let flag = Rect::new(view.x_of(m.position), lane.y + 2.0, 40.0, lane.h - 4.0);
    let glyph = ArrangerView::lane_lock_rect(flag);
    let p = painted(&mut view, &s);
    assert!(
        p.ops
            .iter()
            .any(|o| matches!(o, DrawOp::Rounded(r) if glyph.contains(r.center()))),
        "a padlock before its name"
    );
    let name_x = p.ops.iter().find_map(|o| match o {
        DrawOp::Text(t, r) if *t == m.name => Some(r.x),
        _ => None,
    });
    assert!(
        name_x.is_some_and(|x| x >= glyph.right()),
        "the name after it"
    );
    let tip = view.global_tooltip(GlobalHit::Marker(m.id), &s).unwrap();
    assert!(tip.contains("locked to time"), "{tip}");
    let HostRequest::ContextMenu { items, .. } =
        view.global_menu(GlobalHit::Marker(m.id), &s, Point::new(0.0, 0.0))
    else {
        panic!("the marker menu")
    };
    assert_eq!(lock_item(items).checked, Some(true));
    // A section the same way.
    let section = s.project().sections[0].clone();
    let HostRequest::ContextMenu { items, .. } = view.global_menu(
        GlobalHit::Section(section.id, SectionPart::Body),
        &s,
        Point::new(0.0, 0.0),
    ) else {
        panic!("the section menu")
    };
    s.dispatch(lock_item(items).action.unwrap()).unwrap();
    let rate = f64::from(s.project().sample_rate);
    let at = |s: &Session| {
        let p = s.project();
        let x = p.sections.iter().find(|x| x.id == section.id).unwrap();
        let mk = p.markers.iter().find(|x| x.id == marker.id).unwrap();
        (
            p.timeline.to_samples(x.start, rate),
            p.timeline.to_samples(x.end, rate),
            p.timeline.to_samples(mk.position, rate),
        )
    };
    let before = at(&s);
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 97.0 }))
        .unwrap();
    assert_eq!(at(&s), before, "kept where they are");
    let tip = view
        .global_tooltip(GlobalHit::Section(section.id, SectionPart::Body), &s)
        .unwrap();
    assert!(tip.contains("locked to time"), "{tip}");
}

/// The track menu chooses an instrument track's articulation and drum
/// maps (new ones from templates and the General MIDI key map); audio
/// tracks have neither.
#[test]
fn the_track_menu_chooses_articulation_and_drum_maps() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let submenu = |s: &Session, t: TrackId, title: &str| -> Option<Vec<MenuItem<Action>>> {
        let track = s.project().track(t).unwrap().clone();
        let HostRequest::ContextMenu { items, .. } =
            ArrangerView::track_menu(s, &track, Point::new(0.0, 0.0))
        else {
            panic!("the track menu")
        };
        items
            .into_iter()
            .find(|i| i.label == title)
            .map(|i| i.children)
    };
    let keys = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap()
        .id;
    let audio = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap()
        .id;
    assert!(submenu(&s, audio, "Drum Map").is_none());
    let maps = submenu(&s, keys, "Articulation Map").unwrap();
    assert_eq!(maps[0].checked, Some(true), "none yet");
    let strings = maps.iter().find(|i| i.label == "New: Strings").unwrap();
    s.dispatch(strings.action.clone().unwrap()).unwrap();
    let maps = submenu(&s, keys, "Articulation Map").unwrap();
    assert!(
        maps.iter()
            .any(|i| i.label == "Strings" && i.checked == Some(true))
    );
    let drums = submenu(&s, keys, "Drum Map").unwrap();
    let gm = drums
        .iter()
        .find(|i| i.label == "New: General MIDI")
        .unwrap();
    s.dispatch(gm.action.clone().unwrap()).unwrap();
    assert_eq!(
        s.project().track_drum_map(keys).map(|m| m.name.as_str()),
        Some("General MIDI")
    );
}
