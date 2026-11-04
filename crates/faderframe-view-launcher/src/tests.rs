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

#[test]
fn clips_dropped_from_the_arranger_land_in_the_slot() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Launcher(LauncherOp::AddScene { after: None }))
        .unwrap();
    let scene = s.project().launcher.scenes[0].id;
    let drums = track(&s, "Drums");
    let clip = s.project().clips_of(drums)[0].id;
    let mut view = LauncherView::new(Theme::default());
    let size = Size::new(1200.0, 500.0);
    let tracks: Vec<TrackId> = s.launcher_tracks().iter().map(|t| t.id).collect();
    let col = tracks.iter().position(|t| *t == drums).unwrap();
    let at = Point::new(view.col_x(col) + COL_W / 2.0, view.row_y(0) + ROW_H / 2.0);
    let payload = faderframe_session::launcher::clips_payload(&[clip]);
    assert!(view.hover_payload(Some((&payload, at)), size, &s));
    assert!(view.hover_payload(Some(("something else", at)), size, &s));
    let action = view.drop_payload(&payload, at, size, &s).unwrap();
    assert_eq!(
        action,
        Action::Launcher(LauncherOp::PlaceClips {
            clips: vec![clip],
            track: drums,
            scene
        })
    );
    s.dispatch(action).unwrap();
    let placed = s.project().launcher.clip(drums, scene).unwrap();
    assert_ne!(placed, clip, "a copy");
    assert_eq!(
        s.project().clips_of(drums)[0].id,
        clip,
        "the arrangement keeps it"
    );
    // Audio does not go on a MIDI track.
    let chords = track(&s, "Chords");
    assert!(
        s.dispatch(Action::Launcher(LauncherOp::PlaceClips {
            clips: vec![clip],
            track: chords,
            scene
        }))
        .is_err()
    );
}

mod keyboard {
    use super::*;
    use crate::access::{Seen, launch_news};
    use faderframe_ui_canvas::access::{find_node, focus_order, press_key};
    use faderframe_ui_canvas::{AccessNode, AccessRole, Key};

    const SIZE: Size = Size::new(1200.0, 500.0);

    /// The demo with a drum clip and a chord clip in the launcher (scene
    /// 1), and a second, empty scene.
    fn launcher() -> (Session, TrackId, TrackId) {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let drums = track(&s, "Drums");
        let chords = track(&s, "Chords");
        let clips = vec![
            s.project().clips_of(drums)[0].id,
            s.project().clips_of(chords)[0].id,
        ];
        s.dispatch(Action::Launcher(LauncherOp::SendClips(clips)))
            .unwrap();
        s.dispatch(Action::Launcher(LauncherOp::AddScene { after: None }))
            .unwrap();
        (s, drums, chords)
    }

    fn named(view: &LauncherView, s: &Session, label: &str) -> AccessNode<Action> {
        let nodes = view.accessible(SIZE, s);
        focus_order(&nodes)
            .into_iter()
            .find(|n| n.label.starts_with(label))
            .cloned()
            .unwrap_or_else(|| panic!("no control '{label}'"))
    }

    fn key(
        view: &mut LauncherView,
        s: &Session,
        label: &str,
        k: Key,
    ) -> (Vec<Action>, Vec<HostRequest<Action>>) {
        let id = named(view, s, label).id;
        let nodes = view.accessible(SIZE, s);
        press_key(view, &nodes, id, k, Modifiers::NONE, SIZE, s)
            .unwrap_or_else(|| panic!("{k:?} does something on '{label}'"))
    }

    fn menu(requests: Vec<HostRequest<Action>>) -> Vec<MenuItem<Action>> {
        match requests.into_iter().next() {
            Some(HostRequest::ContextMenu { items, .. }) => items,
            _ => panic!("a menu"),
        }
    }

    /// The toolbar, the tracks, a group per scene (launch, name, a slot per
    /// track) and the stop buttons; Enter launches what a click launches,
    /// the Menu key opens every menu.
    #[test]
    fn the_grid_reads_as_scenes_of_slots_the_keyboard_launches() {
        let (mut s, drums, chords) = launcher();
        let mut view = LauncherView::new(Theme::default());
        assert_eq!(view.accessible_name().as_deref(), Some("Clip Launcher"));
        let nodes = view.accessible(SIZE, &s);
        let roles: Vec<AccessRole> = nodes.iter().map(|n| n.role).collect();
        assert_eq!(
            roles,
            [
                AccessRole::Toolbar,
                AccessRole::Group,
                AccessRole::Group,
                AccessRole::Group,
                AccessRole::Group
            ],
            "toolbar, tracks, two scenes, the stops"
        );
        let scene = s.project().launcher.scenes[0].clone();
        assert_eq!(nodes[2].label, format!("Scene 1: {}", scene.name));
        let tracks = s.launcher_tracks().len();
        assert_eq!(nodes[2].children.len(), 2 + tracks, "launch, name, slots");
        // The slots read as their track and clip; an empty one says so.
        let drum_clip = s.project().launcher.clip(drums, scene.id).unwrap();
        let drum_name = s.project().clip(drum_clip).unwrap().name.clone();
        let slot = named(&view, &s, &format!("Drums: {drum_name}"));
        assert!(slot.menu);
        assert_eq!(slot.selected, Some(false));
        assert_eq!(
            slot.activate,
            Some(Action::Launcher(LauncherOp::Launch {
                track: drums,
                scene: scene.id
            }))
        );
        let empty = view.accessible(SIZE, &s)[3]
            .children
            .iter()
            .find(|n| n.label == "Drums: empty")
            .cloned()
            .unwrap();
        assert!(
            empty.activate.is_none(),
            "an audio track's empty slot: its menu"
        );
        // An empty slot of a MIDI track makes a clip.
        let chord_name = s.project().track(chords).unwrap().name.clone();
        let empty_chords = view.accessible(SIZE, &s)[3]
            .children
            .iter()
            .find(|n| n.label == format!("{chord_name}: empty"))
            .cloned()
            .unwrap();
        assert!(matches!(
            empty_chords.activate,
            Some(Action::Launcher(LauncherOp::CreateClip { track, .. })) if track == chords
        ));
        // The scene's launch button and its name (Enter renames).
        let (actions, _) = key(
            &mut view,
            &s,
            &format!("Launch scene {}", scene.name),
            Key::Enter,
        );
        assert_eq!(
            actions,
            [Action::Launcher(LauncherOp::LaunchScene(scene.id))]
        );
        let (_, req) = key(&mut view, &s, "Scene name: ", Key::Enter);
        let Some(HostRequest::TextInput { commit, .. }) = req.into_iter().next() else {
            panic!("a text entry")
        };
        s.dispatch(commit("Intro").unwrap()).unwrap();
        assert_eq!(s.project().launcher.scenes[0].name, "Intro");
        assert!(named(&view, &s, "Scene name: Intro").press);
        // A slot's menu: launch mode, follow action, moving the clip.
        let (_, req) = key(&mut view, &s, &format!("Drums: {drum_name}"), Key::Menu);
        let items = menu(req);
        for label in ["Launch", "Launch Mode: Trigger", "Move To", "Copy To"] {
            assert!(items.iter().any(|i| i.label == label), "{label}");
        }
        // The toolbar: quantise menu, Record to Arrangement as a toggle.
        let (_, req) = key(&mut view, &s, "Launch quantize:", Key::Enter);
        assert!(!menu(req).is_empty());
        let record = named(&view, &s, "Record to Arrangement");
        assert_eq!(record.role, AccessRole::ToggleButton);
        assert_eq!(record.checked, Some(false));
        let (actions, _) = key(&mut view, &s, "Record to Arrangement", Key::Enter);
        for a in actions {
            s.dispatch(a).unwrap();
        }
        assert_eq!(
            named(&view, &s, "Record to Arrangement").checked,
            Some(true)
        );
        // The stop buttons and the headers.
        let (actions, _) = key(&mut view, &s, "Stop Drums", Key::Enter);
        assert_eq!(actions, [Action::Launcher(LauncherOp::StopTrack(drums))]);
        let (_, req) = key(&mut view, &s, "Stop All Clips", Key::Menu);
        assert!(menu(req).iter().any(|i| i.label == "Stop All Clips"));
        let (actions, _) = key(&mut view, &s, "Drums, plays the arrangement", Key::Enter);
        for a in actions {
            s.dispatch(a).unwrap();
        }
        assert_eq!(
            named(&view, &s, "Drums, plays").selected,
            Some(true),
            "the header selects its track"
        );
        // Toolbar and stops stay put; the scene column stays put sideways.
        let nodes = view.accessible(SIZE, &s);
        assert!(nodes[0].children.iter().all(|n| n.fixed_x && n.fixed_y));
        assert!(nodes[2].children[0].fixed_x && !nodes[2].children[0].fixed_y);
        assert!(find_node(&nodes, slot.id).is_some_and(|n| !n.fixed_x && !n.fixed_y));
    }

    /// "Move To" and "Copy To" put a clip in another slot, as a drag does.
    #[test]
    fn a_clip_moves_to_another_slot_from_its_menu() {
        let (mut s, drums, _) = launcher();
        let mut view = LauncherView::new(Theme::default());
        let scenes: Vec<SceneId> = s.project().launcher.scenes.iter().map(|x| x.id).collect();
        let clip = s.project().launcher.clip(drums, scenes[0]).unwrap();
        let name = s.project().clip(clip).unwrap().name.clone();
        let (_, req) = key(&mut view, &s, &format!("Drums: {name}"), Key::Menu);
        let items = menu(req);
        let to = items.iter().find(|i| i.label == "Move To").unwrap();
        let drum_slots = to.children.iter().find(|i| i.label == "Drums").unwrap();
        assert_eq!(drum_slots.children.len(), 1, "the other scene");
        s.dispatch(drum_slots.children[0].action.clone().unwrap())
            .unwrap();
        assert!(s.project().launcher.clip(drums, scenes[0]).is_none());
        assert!(s.project().launcher.clip(drums, scenes[1]).is_some());
    }

    /// Launches, stops, the way back to the arrangement and slot
    /// recordings are said as they happen; nothing on the first look.
    #[test]
    fn launches_and_stops_are_said() {
        let (s, drums, chords) = launcher();
        let mut view = LauncherView::new(Theme::default());
        assert!(view.access_news(&s).is_empty());
        assert!(view.access_news(&s).is_empty());
        let scene = s.project().launcher.scenes[0].id;
        let slot = SlotKey {
            track: drums,
            scene,
        }
        .hash();
        let name = s
            .project()
            .clip(s.project().launcher.clip(drums, scene).unwrap())
            .unwrap()
            .name
            .clone();
        let idle = Seen {
            tracks: vec![(drums, None, true), (chords, None, true)],
            recording: None,
        };
        let playing = Seen {
            tracks: vec![(drums, Some(slot), false), (chords, None, true)],
            recording: None,
        };
        assert_eq!(
            launch_news(&idle, &playing, &s),
            [format!("{name} playing on Drums")]
        );
        let stopped = Seen {
            tracks: vec![(drums, None, false), (chords, None, true)],
            recording: None,
        };
        assert_eq!(launch_news(&playing, &stopped, &s), ["Drums stopped"]);
        assert_eq!(
            launch_news(&stopped, &idle, &s),
            ["Back to the arrangement: Drums"]
        );
        let recording = Seen {
            recording: Some((chords, scene)),
            ..idle.clone()
        };
        let chord_name = s.project().track(chords).unwrap().name.clone();
        let scene_name = s.project().launcher.scenes[0].name.clone();
        assert_eq!(
            launch_news(&idle, &recording, &s),
            [format!("Recording into {chord_name}, {scene_name}")]
        );
        assert_eq!(
            launch_news(&recording, &idle, &s),
            [format!("Recording into {chord_name}, {scene_name} ended")]
        );
    }
}

#[test]
fn sounds_dropped_from_the_browser_land_in_the_slot() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Launcher(LauncherOp::AddScene { after: None }))
        .unwrap();
    let scene = s.project().launcher.scenes[0].id;
    let drums = track(&s, "Drums");
    let mut view = LauncherView::new(Theme::default());
    let size = Size::new(1200.0, 500.0);
    let tracks: Vec<TrackId> = s.launcher_tracks().iter().map(|t| t.id).collect();
    let col = tracks.iter().position(|t| *t == drums).unwrap();
    let at = Point::new(view.col_x(col) + COL_W / 2.0, view.row_y(0) + ROW_H / 2.0);
    let file = std::path::PathBuf::from("/samples/Loop 120.wav");
    let payload = faderframe_session::browser::payload(std::slice::from_ref(&file));
    assert!(view.hover_payload(Some((&payload, at)), size, &s));
    assert_eq!(
        view.drop_payload(&payload, at, size, &s),
        Some(Action::Browser(
            faderframe_session::browser::BrowserOp::ToLauncher {
                files: vec![file],
                track: drums,
                scene
            }
        ))
    );
}
