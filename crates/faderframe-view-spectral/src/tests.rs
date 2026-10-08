#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SR: u32 = 48_000;
const SIZE: Size = Size::new(1200.0, 520.0);

fn session() -> (Session, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-view-spectral-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Tone.wav");
    let tone: Vec<f32> = (0..2 * SR as usize)
        .map(|i| (0.25 * (i as f64 * 2000.0 * std::f64::consts::TAU / f64::from(SR)).sin()) as f32)
        .collect();
    write_wav(&file, &[tone], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Spectral", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    s.dispatch(Action::OpenSpectralEditor(clip)).unwrap();
    (s, clip)
}

fn run(
    view: &mut SpectralView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn press(pos: Point) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    }
}

fn drag(view: &mut SpectralView, s: &Session, from: Point, to: Point) -> Vec<Action> {
    let mut all = run(view, press(from), s).0;
    for k in 1..=10 {
        let t = k as f32 / 10.0;
        let p = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        all.extend(
            run(
                view,
                ViewEvent::PointerMove {
                    pos: p,
                    modifiers: Modifiers::NONE,
                    dragging: true,
                },
                s,
            )
            .0,
        );
    }
    all.extend(
        run(
            view,
            ViewEvent::PointerUp {
                pos: to,
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            s,
        )
        .0,
    );
    all
}

#[test]
fn a_region_drawn_becomes_an_edit_and_can_be_selected_moved_and_removed() {
    let (mut s, clip) = session();
    let mut view = SpectralView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"Spectral"));
    let l = SpectralView::layout(SIZE);
    let area = l.area;
    // A rectangle over the middle second, around 2 kHz.
    let shown = view.shown(&s).unwrap();
    let (y1, y2) = (
        SpectralView::y_of(&shown, area, 3000.0),
        SpectralView::y_of(&shown, area, 1400.0),
    );
    let (x1, x2) = (area.x + area.w * 0.25, area.x + area.w * 0.75);
    let actions = drag(&mut view, &s, Point::new(x1, y1), Point::new(x2, y2));
    assert!(actions.is_empty(), "drawing alone edits nothing");
    let Some(SpectralShape::Rect {
        start,
        end,
        low,
        high,
    }) = view.selection.clone()
    else {
        panic!("a rectangle drawn: {:?}", view.selection);
    };
    assert!(
        (start - 24_000).abs() < 200 && (end - 72_000).abs() < 200,
        "{start}–{end}"
    );
    assert!(
        (low - 1400.0).abs() < 60.0 && (high - 3000.0).abs() < 120.0,
        "{low}–{high}"
    );
    // Remove: an edit of it.
    let (actions, _) = run(&mut view, press(l.ops[2].center()), &s);
    let [
        Action::EditSpectral {
            clip: c,
            change: SpectralChange::Add(edit),
        },
    ] = actions.as_slice()
    else {
        panic!("{actions:?}");
    };
    assert_eq!(*c, clip);
    assert_eq!(edit.op, SpectralOp::Remove);
    assert_eq!((edit.feather_ms, edit.feather_st), (10.0, 1.0));
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(view.selected, Some(0));
    assert!(view.selection.is_none());
    // Heal changes the selected edit's operation.
    let (actions, _) = run(&mut view, press(l.ops[1].center()), &s);
    assert!(matches!(
        actions.as_slice(),
        [Action::EditSpectral {
            change: SpectralChange::Set(
                0,
                SpectralEdit {
                    op: SpectralOp::Heal,
                    ..
                }
            ),
            ..
        }]
    ));
    for a in actions {
        s.dispatch(a).unwrap();
    }
    // Moved by a drag: later in time.
    let inside = Point::new((x1 + x2) / 2.0, (y1 + y2) / 2.0);
    let actions = drag(
        &mut view,
        &s,
        inside,
        Point::new(inside.x + area.w * 0.1, inside.y),
    );
    let [
        Action::EditSpectral {
            change: SpectralChange::Set(0, moved),
            ..
        },
    ] = actions.as_slice()
    else {
        panic!("{actions:?}");
    };
    let (a, _) = moved.shape.frames();
    assert!((a - 24_000 - 9_600).abs() < 300, "{a}");
    for a in actions {
        s.dispatch(a).unwrap();
    }
    // A click elsewhere selects nothing; a click on it selects it; Delete.
    run(
        &mut view,
        press(Point::new(area.x + 10.0, area.y + 10.0)),
        &s,
    );
    run(
        &mut view,
        ViewEvent::PointerUp {
            pos: Point::new(area.x + 10.0, area.y + 10.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        &s,
    );
    assert_eq!(view.selected, None);
    let on = Point::new(inside.x + area.w * 0.1, inside.y);
    run(&mut view, press(on), &s);
    run(
        &mut view,
        ViewEvent::PointerUp {
            pos: on,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        &s,
    );
    assert_eq!(view.selected, Some(0));
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Delete,
            modifiers: Modifiers::NONE,
        },
        &s,
    );
    assert!(matches!(
        actions.as_slice(),
        [Action::EditSpectral {
            change: SpectralChange::Remove(0),
            ..
        }]
    ));
}

#[test]
fn the_tools_draw_their_shapes() {
    let (s, _) = session();
    let mut view = SpectralView::new(Theme::default());
    let l = SpectralView::layout(SIZE);
    let area = l.area;
    let shown = view.shown(&s).unwrap();
    // Time: every frequency.
    run(&mut view, press(l.tools[1].center()), &s);
    assert_eq!(view.tool, Tool::Time);
    let y = area.y + area.h / 2.0;
    drag(
        &mut view,
        &s,
        Point::new(area.x + 100.0, y),
        Point::new(area.x + 200.0, y + 5.0),
    );
    let Some(SpectralShape::Rect { low, high, .. }) = view.selection.clone() else {
        panic!();
    };
    assert_eq!((low, high), (0.0, 24_000.0));
    // Band: the whole clip.
    run(&mut view, press(l.tools[2].center()), &s);
    drag(
        &mut view,
        &s,
        Point::new(area.x + 300.0, y - 40.0),
        Point::new(area.x + 320.0, y + 40.0),
    );
    let Some(SpectralShape::Rect { start, end, .. }) = view.selection.clone() else {
        panic!();
    };
    assert_eq!((start, end), (0, 96_000));
    // Lasso and brush: their points.
    run(&mut view, press(l.tools[3].center()), &s);
    drag(
        &mut view,
        &s,
        Point::new(area.x + 100.0, y - 40.0),
        Point::new(area.x + 400.0, y + 40.0),
    );
    assert!(matches!(&view.selection, Some(SpectralShape::Lasso { points }) if points.len() > 5));
    run(&mut view, press(l.tools[4].center()), &s);
    drag(
        &mut view,
        &s,
        Point::new(area.x + 100.0, y),
        Point::new(area.x + 400.0, y),
    );
    let Some(SpectralShape::Brush {
        radius_ms,
        radius_st,
        ..
    }) = view.selection.clone()
    else {
        panic!();
    };
    // 12 px of a 2 s view and of the frequency axis.
    let ms = 12.0 / area.w * 2000.0;
    assert!((radius_ms - ms).abs() < 0.5, "{radius_ms} vs {ms}");
    assert!((radius_st - 12.0 / SpectralView::px_per_semitone(&shown, area)).abs() < 0.01);
}

#[test]
fn the_picture_arrives_and_zoom_keeps_the_pointer() {
    let (s, _) = session();
    let mut view = SpectralView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let l = SpectralView::layout(SIZE);
    let shown = view.shown(&s).unwrap();
    let start = std::time::Instant::now();
    while view.picture.is_none() {
        assert!(start.elapsed().as_secs() < 30, "no picture");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
    }
    // Ctrl+wheel: zoomed in around the pointer.
    let at = Point::new(l.area.x + l.area.w * 0.3, l.area.y + 50.0);
    let before = view.frame_at(&shown, l.area, at.x);
    run(
        &mut view,
        ViewEvent::Scroll {
            pos: at,
            dx: 0.0,
            dy: -2.0,
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
            precise: false,
        },
        &s,
    );
    let (a, b) = view.visible(&shown);
    assert!(b - a < 96_000.0 * 0.7, "zoomed in: {}", b - a);
    assert!((view.frame_at(&shown, l.area, at.x) - before).abs() < 10.0);
}
