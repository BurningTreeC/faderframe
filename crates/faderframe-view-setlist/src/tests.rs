#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SIZE: Size = Size::new(1200.0, 700.0);

fn session() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Setlist(SetlistOp::FromSections))
        .unwrap();
    s
}

fn run(
    view: &mut SetlistView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn press(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

#[test]
fn the_list_shows_the_songs_and_edits_them() {
    let mut s = session();
    let mut view = SetlistView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(
        texts.contains(&"Intro") && texts.contains(&"Verse"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.starts_with("2 songs")));
    // Renaming by a double-click on the name.
    let name = SetlistView::cell(1, view.row_rect(0, SIZE)).center();
    let (_, requests) = run(&mut view, press(name, 2), &s);
    let Some(HostRequest::TextInput { commit, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("a text entry");
    };
    s.dispatch(commit("Opener").unwrap()).unwrap();
    assert_eq!(s.setlist().songs[0].name, "Opener");
    // What follows: from the Then column's menu.
    let then = SetlistView::cell(4, view.row_rect(0, SIZE)).center();
    let (_, requests) = run(&mut view, press(then, 1), &s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("the menu");
    };
    let on = items.iter().find(|i| i.label == "Play on").unwrap();
    s.dispatch(on.action.clone().unwrap()).unwrap();
    assert_eq!(s.setlist().songs[0].then, AfterSong::Continue);
    // Alt+Down moves the selected song.
    let (actions, _) = run(
        &mut view,
        ViewEvent::Key {
            key: Key::Down,
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
        },
        &s,
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.setlist().songs[1].name, "Opener");
}

#[test]
fn the_stage_screen_plays_the_show() {
    let mut s = session();
    let mut view = SetlistView::new(Theme::default());
    // Show Mode from the header.
    let (_, show, _) = SetlistView::buttons(SIZE)[2];
    let (actions, _) = run(&mut view, press(show.center(), 1), &s);
    assert_eq!(actions, [Action::Show(ShowOp::Enter)]);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(s.show_mode());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let texts = p.texts();
    assert!(texts.contains(&"SHOW"), "{texts:?}");
    assert!(texts.contains(&"Intro"));
    assert!(texts.contains(&"NEXT"), "the next song marked");
    assert!(texts.iter().any(|t| t.starts_with("Song 1 of 2")));
    // The keys: → the next song, Space plays, Esc leaves.
    for (key, op) in [
        (Key::Right, ShowOp::Next),
        (Key::Space, ShowOp::PlayStop),
        (Key::Escape, ShowOp::Leave),
    ] {
        let (actions, _) = run(
            &mut view,
            ViewEvent::Key {
                key,
                modifiers: Modifiers::NONE,
            },
            &s,
        );
        assert_eq!(actions, [Action::Show(op)]);
    }
    // The buttons.
    let l = SetlistView::stage(SIZE);
    assert_eq!(view.hit(l.next.center(), SIZE, &s), Some(Hit::Next));
    assert_eq!(view.hit(l.play.center(), SIZE, &s), Some(Hit::PlayStop));
    let row = SetlistView::stage_row(&l, 1).center();
    let (actions, _) = run(&mut view, press(row, 2), &s);
    assert_eq!(actions, [Action::Show(ShowOp::Go(1))]);
}

/// A song's context menu names the mix scene recalled when the show
/// stands on it; the list shows it beside what follows the song.
#[test]
fn a_song_chooses_its_mix_scene() {
    use faderframe_session::scenes::SceneOp;
    let mut s = session();
    s.dispatch(Action::MixScene(SceneOp::Store {
        name: Some("Loud".into()),
        plugins: false,
    }))
    .unwrap();
    let id = s.mix_scenes().scenes[0].id;
    let mut view = SetlistView::new(Theme::default());
    let row = view.row_rect(0, SIZE).center();
    let (_, requests) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: row,
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
        panic!("the menu");
    };
    let scenes = &items
        .iter()
        .find(|i| i.label == "Mix Scene")
        .unwrap()
        .children;
    assert_eq!(scenes[0].checked, Some(true), "none yet");
    let loud = scenes.iter().find(|i| i.label == "Loud").unwrap();
    s.dispatch(loud.action.clone().unwrap()).unwrap();
    assert_eq!(s.setlist().songs[0].scene, Some(id));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(
        p.texts().iter().any(|t| t.ends_with("· ‘Loud’")),
        "{:?}",
        p.texts()
    );
}

#[test]
fn the_gear_column_types_gear_changes_and_its_menu_does_the_rest() {
    let mut s = session();
    let _out = s.add_virtual_midi_output("Synth");
    let mut view = SetlistView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.texts().contains(&"Gear"));
    // A double-click types them.
    let cell = SetlistView::cell(GEAR, view.row_rect(1, SIZE)).center();
    let (_, requests) = run(&mut view, press(cell, 2), &s);
    let Some(HostRequest::TextInput {
        commit, initial, ..
    }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("a text entry");
    };
    assert!(initial.is_empty());
    s.dispatch(commit("pc 5 bank 1; cc 7=100 ch 2").unwrap())
        .unwrap();
    let gear = &s.setlist().songs[1].gear;
    assert_eq!(gear.changes.len(), 2);
    assert_eq!(gear.changes[0].output, "virtual:Synth");
    // Shown in the row.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(
        p.texts().iter().any(|t| t.starts_with("PC 5 bank 1 ch 1")),
        "{:?}",
        p.texts()
    );
    // A click: the menu, listing them, the timing, Send Now, patch dumps,
    // snapshots.
    let (_, requests) = run(&mut view, press(cell, 1), &s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels[0].starts_with("PC 5 bank 1"), "{labels:?}");
    let start = items
        .iter()
        .find(|i| i.label == "Sent On Its First Frame")
        .unwrap();
    s.dispatch(start.action.clone().unwrap()).unwrap();
    assert_eq!(s.setlist().songs[1].gear.timing, GearTiming::Start);
    assert!(labels.contains(&"Send Now"));
    let dump = items
        .iter()
        .find(|i| i.label.starts_with("Add Patch Dump"))
        .unwrap();
    assert!(matches!(
        dump.action,
        Some(Action::Setlist(SetlistOp::ChooseGearSysex(1)))
    ));
    assert!(labels.contains(&"Add Controller Snapshot of"));
    // The row's menu has it too.
    let menu = SetlistView::row_menu(&s, 1);
    assert!(menu.iter().any(|i| i.label == "External Gear"));
    let clear = SetlistView::gear_items(&s, 1)
        .into_iter()
        .find(|i| i.label == "Clear Gear Changes")
        .unwrap();
    s.dispatch(clear.action.unwrap()).unwrap();
    assert!(s.setlist().songs[1].gear.changes.is_empty());
}
