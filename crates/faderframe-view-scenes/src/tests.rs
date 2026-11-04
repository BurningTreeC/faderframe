#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, Track, TrackColor, TrackKind};
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SIZE: Size = Size::new(1100.0, 420.0);

fn session() -> (Session, TrackId) {
    let mut p = Project::new("Scenes", 48_000);
    let vox: TrackId = p.ids.allocate();
    p.tracks.insert(
        0,
        Track::new(vox, TrackKind::Audio, "Vox", TrackColor::palette(0)),
    );
    let s = Session::new(p, None, EngineConfig::default()).unwrap();
    (s, vox)
}

fn store(s: &mut Session, name: &str) -> MixSceneId {
    s.dispatch(Action::MixScene(SceneOp::Store {
        name: Some(name.into()),
        plugins: false,
    }))
    .unwrap();
    s.mix_scenes().scenes.last().unwrap().id
}

fn press(
    view: &mut ScenesView,
    s: &Session,
    pos: Point,
    button: PointerButton,
    clicks: u32,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
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
        SIZE,
        s,
        &mut cx,
    );
    (actions, requests)
}

fn send(view: &mut ScenesView, s: &Session, ev: ViewEvent) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    actions
}

fn button(s: &Session, hit: Hit) -> Point {
    ScenesView::buttons(SIZE, s)
        .into_iter()
        .chain(ScenesView::strip(SIZE, s))
        .find(|(h, _)| *h == hit)
        .unwrap()
        .1
        .center()
}

#[test]
fn an_empty_view_says_how_to_begin_and_stores() {
    let (s, _) = session();
    let mut view = ScenesView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.balanced_clips());
    let texts = p.texts();
    for t in [
        "Mix Scenes",
        "+ Store",
        "+ Store with Plugins",
        "Crossfade: Instant ▾",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    assert!(texts.iter().any(|t| t.starts_with("No scenes yet")));
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::Store),
        PointerButton::Primary,
        1,
    );
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::Store {
            name: None,
            plugins: false
        })]
    );
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::StorePlugins),
        PointerButton::Primary,
        1,
    );
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::Store {
            name: None,
            plugins: true
        })]
    );
}

#[test]
fn the_list_chooses_recalls_and_offers_a_menu() {
    let (mut s, vox) = session();
    let verse = store(&mut s, "Verse");
    let chorus = store(&mut s, "Chorus");
    let mut view = ScenesView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    for t in [
        "Verse",
        "Chorus",
        "‘Chorus’ recalls",
        "Faders",
        "Vox",
        "Its recall",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    // A click chooses (no action), a double click recalls.
    let row = ScenesView::row_rect(0, SIZE).center();
    let (actions, _) = press(&mut view, &s, row, PointerButton::Primary, 1);
    assert!(actions.is_empty());
    assert_eq!(view.shown(&s), Some(verse));
    let (actions, _) = press(&mut view, &s, row, PointerButton::Primary, 2);
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::Recall {
            scene: verse,
            fade: None
        })]
    );
    // The header's Recall recalls the chosen one; Enter too.
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::Recall),
        PointerButton::Primary,
        1,
    );
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::Recall {
            scene: verse,
            fade: None
        })]
    );
    let moved = send(
        &mut view,
        &s,
        ViewEvent::Key {
            key: Key::Down,
            modifiers: Modifiers::NONE,
        },
    );
    assert!(moved.is_empty());
    assert_eq!(view.shown(&s), Some(chorus));
    // The row's menu.
    let (_, requests) = press(&mut view, &s, row, PointerButton::Secondary, 1);
    let Some(HostRequest::ContextMenu { items, .. }) = requests.first() else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    for l in [
        "Recall (Instant)",
        "Recall at Once",
        "Rename…",
        "Update from the Console",
        "Make It A",
        "Compare with the Mix",
        "Move Down",
        "MIDI Control",
        "Delete Scene",
    ] {
        assert!(labels.contains(&l), "{l} in {labels:?}");
    }
    // Scope and safe chips toggle the chosen scene.
    let chips = view.chips(SIZE, &s);
    let (_, faders, _) = chips
        .iter()
        .find(|(h, _, _)| *h == Hit::Scope(ScopePart::Faders))
        .unwrap();
    let (actions, _) = press(&mut view, &s, faders.center(), PointerButton::Primary, 1);
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::SetScope {
            scene: verse,
            part: ScopePart::Faders,
            on: false
        })]
    );
    let (_, safe, _) = chips.iter().find(|(h, _, _)| *h == Hit::Safe(vox)).unwrap();
    let (actions, _) = press(&mut view, &s, safe.center(), PointerButton::Primary, 1);
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::SetSafe {
            scene: verse,
            track: vox,
            safe: true
        })]
    );
    // A scene stored without plugins: its Plugins chip does nothing.
    let (_, plugins, _) = chips
        .iter()
        .find(|(h, _, _)| *h == Hit::Scope(ScopePart::Plugins))
        .unwrap();
    let (actions, _) = press(&mut view, &s, plugins.center(), PointerButton::Primary, 1);
    assert!(actions.is_empty());
}

/// The morph slider: a press and drags move the morph, the release ends
/// it (one undo step in the session); without A and B it does nothing.
#[test]
fn the_morph_slider_is_one_gesture() {
    let (mut s, vox) = session();
    let a = store(&mut s, "A");
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: vox,
        db: -12.0,
    }))
    .unwrap();
    let b = store(&mut s, "B");
    let mut view = ScenesView::new(Theme::default());
    let slider = ScenesView::strip(SIZE, &s)
        .into_iter()
        .find(|(h, _)| *h == Hit::Morph)
        .unwrap()
        .1;
    let (actions, _) = press(&mut view, &s, slider.center(), PointerButton::Primary, 1);
    assert!(actions.is_empty(), "no A and B yet");
    s.dispatch(Action::MixScene(SceneOp::SetAB {
        a: Some(a),
        b: Some(b),
    }))
    .unwrap();
    let start = Point::new(slider.x + 28.0, slider.center().y);
    let (pressed, _) = press(&mut view, &s, start, PointerButton::Primary, 1);
    assert_eq!(
        pressed,
        [Action::MixScene(SceneOp::Morph { position: 0.0 })]
    );
    let end = Point::new(slider.right() - 28.0, slider.center().y);
    let moved = send(
        &mut view,
        &s,
        ViewEvent::PointerMove {
            pos: end,
            modifiers: Modifiers::NONE,
            dragging: true,
        },
    );
    assert_eq!(moved, [Action::MixScene(SceneOp::Morph { position: 1.0 })]);
    let up = send(
        &mut view,
        &s,
        ViewEvent::PointerUp {
            pos: end,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
    );
    assert_eq!(up, [Action::MixScene(SceneOp::EndMorph)]);
    // Through the session: one step, B's level (by way of A's).
    let steps = s.history_steps().0.len();
    for a in [pressed, moved, up].concat() {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.history_steps().0.len(), steps + 1);
    assert_eq!(s.project().track(vox).unwrap().volume_db, -12.0);
    // Right-click: MIDI learn for the morph.
    let (_, requests) = press(&mut view, &s, slider.center(), PointerButton::Secondary, 1);
    let Some(HostRequest::ContextMenu { items, .. }) = requests.first() else {
        panic!("a menu");
    };
    assert_eq!(items[0].label, "MIDI Learn…");
}

/// The compare halves start a compare of A (the mix when none) and B,
/// then switch sides; Keep and Back show while comparing.
#[test]
fn the_compare_halves_start_and_switch() {
    let (mut s, vox) = session();
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: vox,
        db: -6.0,
    }))
    .unwrap();
    let b = store(&mut s, "B");
    s.dispatch(Action::MixScene(SceneOp::SetAB {
        a: None,
        b: Some(b),
    }))
    .unwrap();
    let mut view = ScenesView::new(Theme::default());
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::HearB),
        PointerButton::Primary,
        1,
    );
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::CompareStart { a: None, b })]
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(
        ScenesView::strip(SIZE, &s)
            .iter()
            .any(|(h, _)| *h == Hit::Keep)
    );
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(
        p.texts()
            .iter()
            .any(|t| t.starts_with("Comparing · hearing ‘B’")),
        "{:?}",
        p.texts()
    );
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::HearA),
        PointerButton::Primary,
        1,
    );
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::CompareHear(SceneSide::A))]
    );
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::Back),
        PointerButton::Primary,
        1,
    );
    assert_eq!(
        actions,
        [Action::MixScene(SceneOp::CompareEnd { keep: false })]
    );
}

/// The project's crossfade: a menu of lengths, a typed one on a
/// double-click; a crossfade under way shows Stop and its progress.
#[test]
fn the_crossfade_is_chosen_and_shown() {
    let (mut s, vox) = session();
    let a = store(&mut s, "A");
    let mut view = ScenesView::new(Theme::default());
    let (_, requests) = press(
        &mut view,
        &s,
        button(&s, Hit::Fade),
        PointerButton::Primary,
        1,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests.first() else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels[..3], ["Instant", "250 ms", "500 ms"]);
    assert!(labels.contains(&"4 beats"));
    let (_, requests) = press(
        &mut view,
        &s,
        button(&s, Hit::Fade),
        PointerButton::Primary,
        2,
    );
    let Some(HostRequest::TextInput { commit, .. }) = requests.into_iter().next() else {
        panic!("a text entry");
    };
    assert_eq!(
        commit("3 s"),
        Some(Action::MixScene(SceneOp::SetFade {
            scene: None,
            fade: Some(SceneFade {
                length: FadeLength::Millis { ms: 3000.0 },
                switch: SwitchPoint::Middle,
            }),
        }))
    );
    assert_eq!(commit("loud"), None);
    // A slow crossfade under way.
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: vox,
        db: -20.0,
    }))
    .unwrap();
    s.dispatch(Action::MixScene(SceneOp::Recall {
        scene: a,
        fade: Some(SceneFade {
            length: FadeLength::Millis { ms: 60_000.0 },
            switch: SwitchPoint::End,
        }),
    }))
    .unwrap();
    assert!(view.wants_frames(&s));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"Stop"));
    assert!(
        p.texts()
            .iter()
            .any(|t| t.starts_with("Crossfading to ‘A’"))
    );
    let (actions, _) = press(
        &mut view,
        &s,
        button(&s, Hit::Stop),
        PointerButton::Primary,
        1,
    );
    assert_eq!(actions, [Action::MixScene(SceneOp::StopFade)]);
}
