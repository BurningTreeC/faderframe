#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{CanvasView, Modifiers, RecordingPainter};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn click(
    view: &mut LauncherView,
    s: &Session,
    size: Size,
    pos: Point,
    button: PointerButton,
    clicks: u32,
) -> (Vec<Action>, usize) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerDown {
            pos,
            button,
            modifiers: Modifiers::NONE,
            clicks,
        },
        size,
        s,
        &mut cx,
    );
    let n = requests.len();
    (actions, n)
}

#[test]
fn the_grid_shows_scenes_and_clips_and_clicks_launch_them() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let chords = track(&s, "Chords");
    let first = s.project().clips_of(drums)[0].id;
    s.dispatch(Action::Launcher(LauncherOp::SendClips(vec![first])))
        .unwrap();
    let scene = s.project().launcher.scenes[0].id;
    let mut view = LauncherView::new(Theme::default());
    let size = Size::new(1200.0, 500.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let texts = p.texts();
    for t in [
        "Clip Launcher",
        "Drums",
        "Chords",
        "Scene 1",
        "Back to Arrangement",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    // The drum clip's ▶ launches it; its body selects it.
    let tracks: Vec<TrackId> = s.launcher_tracks().iter().map(|t| t.id).collect();
    let col = tracks.iter().position(|t| *t == drums).unwrap();
    let y = view.row_y(0) + ROW_H / 2.0;
    let play = Point::new(view.col_x(col) + 10.0, y);
    assert_eq!(
        view.hit(play, size, &s),
        Some(Hit::Slot {
            track: drums,
            scene,
            clip: s.project().launcher.clip(drums, scene),
            play: true
        })
    );
    let (actions, _) = click(&mut view, &s, size, play, PointerButton::Primary, 1);
    assert_eq!(
        actions,
        [Action::Launcher(LauncherOp::Launch {
            track: drums,
            scene
        })]
    );
    // The scene's ▶ launches the row.
    let (actions, _) = click(
        &mut view,
        &s,
        size,
        Point::new(14.0, y),
        PointerButton::Primary,
        1,
    );
    assert_eq!(actions, [Action::Launcher(LauncherOp::LaunchScene(scene))]);
    // A double click on an empty slot of a MIDI track makes a clip.
    let col = tracks.iter().position(|t| *t == chords).unwrap();
    let empty = Point::new(view.col_x(col) + COL_W / 2.0, y);
    let (actions, _) = click(&mut view, &s, size, empty, PointerButton::Primary, 2);
    assert_eq!(
        actions,
        [Action::Launcher(LauncherOp::CreateClip {
            track: chords,
            scene
        })]
    );
    // Right-click: a menu.
    let (_, menus) = click(&mut view, &s, size, empty, PointerButton::Secondary, 1);
    assert_eq!(menus, 1);
    // Toolbar: back to the arrangement, record, a scene, stop all.
    let [(_, quantize), (_, back), (_, record), (_, add), (_, stop)] = LauncherView::buttons(size);
    let (actions, menus) = click(
        &mut view,
        &s,
        size,
        quantize.center(),
        PointerButton::Primary,
        1,
    );
    assert!(actions.is_empty() && menus == 1);
    let emitted: Vec<Action> = [back, record, add, stop]
        .iter()
        .flat_map(|r| click(&mut view, &s, size, r.center(), PointerButton::Primary, 1).0)
        .collect();
    assert_eq!(
        emitted,
        [
            Action::Launcher(LauncherOp::BackToArrangement),
            Action::Launcher(LauncherOp::SetRecord(true)),
            Action::Launcher(LauncherOp::AddScene { after: None }),
            Action::Launcher(LauncherOp::StopAll),
        ]
    );
}

#[test]
fn a_clip_drags_to_another_slot() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let first = s.project().clips_of(drums)[0].id;
    s.dispatch(Action::Launcher(LauncherOp::SendClips(vec![first])))
        .unwrap();
    s.dispatch(Action::Launcher(LauncherOp::AddScene { after: None }))
        .unwrap();
    let scenes: Vec<SceneId> = s.project().launcher.scenes.iter().map(|s| s.id).collect();
    let mut view = LauncherView::new(Theme::default());
    let size = Size::new(1200.0, 500.0);
    let tracks: Vec<TrackId> = s.launcher_tracks().iter().map(|t| t.id).collect();
    let col = tracks.iter().position(|t| *t == drums).unwrap();
    let x = view.col_x(col) + COL_W / 2.0;
    let from = Point::new(x, view.row_y(0) + ROW_H / 2.0);
    let to = Point::new(x, view.row_y(1) + ROW_H / 2.0);
    let (actions, _) = click(&mut view, &s, size, from, PointerButton::Primary, 1);
    assert!(matches!(actions[..], [Action::SelectClips { .. }]));
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerMove {
            pos: to,
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        size,
        &s,
        &mut cx,
    );
    view.event(
        &ViewEvent::PointerUp {
            pos: to,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
        &mut cx,
    );
    assert_eq!(
        actions,
        [Action::Launcher(LauncherOp::MoveClip {
            from: SlotKey {
                track: drums,
                scene: scenes[0]
            },
            to: SlotKey {
                track: drums,
                scene: scenes[1]
            },
            copy: false,
        })]
    );
}
