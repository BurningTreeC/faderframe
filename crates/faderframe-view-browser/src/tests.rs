#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};
use std::time::Duration;

const SIZE: Size = Size { w: 420.0, h: 640.0 };

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-view-browser-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(path: &Path, hz: f32, seconds: f32) {
    let n = (48_000.0 * seconds) as usize;
    let x: Vec<f32> = (0..n)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin() * 0.4)
        .collect();
    faderframe_audio_files::write_wav(
        path,
        &[x.clone(), x],
        48_000,
        faderframe_audio_files::WavFormat::Pcm16,
        false,
    )
    .unwrap();
}

/// A session browsing a folder with two sounds and a folder.
fn session(name: &str) -> (Session, PathBuf) {
    let dir = temp(name);
    let media = dir.join("Sounds");
    std::fs::create_dir_all(media.join("Drums")).unwrap();
    tone(&media.join("Bass Hit.wav"), 55.0, 0.5);
    tone(&media.join("Keys.wav"), 440.0, 1.0);
    let mut s = Session::new(
        faderframe_project::Project::new("Browse", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    s.set_browser_dir(dir.join("browser"));
    s.dispatch(Action::Browser(BrowserOp::AddFolder(media.clone())))
        .unwrap();
    assert!(s.wait_for_browser(Duration::from_secs(120)));
    (s, media)
}

fn press(
    view: &mut BrowserView,
    s: &Session,
    pos: Point,
    button: PointerButton,
    clicks: u32,
    modifiers: Modifiers,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerDown {
            pos,
            button,
            modifiers,
            clicks,
        },
        SIZE,
        s,
        &mut cx,
    );
    (actions, requests)
}

fn row_of(s: &Session, name: &str) -> usize {
    s.browser()
        .rows()
        .iter()
        .position(|r| r.name == name)
        .unwrap()
}

#[test]
fn a_folder_lists_its_files_with_what_they_are() {
    let (s, media) = session("list");
    let names: Vec<&str> = s.browser().rows().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["Drums", "Bass Hit.wav", "Keys.wav"]);
    let keys = &s.browser().rows()[row_of(&s, "Keys.wav")];
    assert_eq!(keys.format, "WAV 16-bit");
    assert!((keys.seconds.unwrap() - 1.0).abs() < 1e-3);
    assert!(!keys.pending);
    assert_eq!(keys.thumb_levels().len(), browser::index::THUMB_POINTS);
    let mut view = BrowserView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert!(p.balanced_clips());
    let texts = p.texts();
    for t in [
        "Sounds ▾",
        "▸ Drums",
        "Bass Hit.wav",
        "Keys.wav",
        "1.00 s",
        "All Files ▾",
        "Any Length ▾",
        "Any Key ▾",
        "Sync",
        "Loop",
        "Subfolders",
        "Name ▴",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Select a file to hear it")),
        "{texts:?}"
    );
    // A folder dropped on the view becomes a place.
    let other = temp("dropped");
    let a = view.drop_files(
        std::slice::from_ref(&other),
        Point::new(10.0, 300.0),
        SIZE,
        &s,
    );
    assert_eq!(
        a,
        Some(Action::Several(vec![Action::Browser(
            BrowserOp::AddFolder(other)
        )]))
    );
    let _ = media;
}

#[test]
fn clicks_select_rate_sort_and_open() {
    let (s, media) = session("clicks");
    let mut view = BrowserView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let keys = row_of(&s, "Keys.wav");
    let r = view.row_rect(keys, SIZE, &s);
    // A click selects (the session hears it, Auto being on).
    let (a, _) = press(
        &mut view,
        &s,
        Point::new(r.x + 60.0, r.y + 10.0),
        PointerButton::Primary,
        1,
        Modifiers::NONE,
    );
    assert_eq!(
        a,
        vec![Action::Browser(BrowserOp::Select(vec![
            media.join("Keys.wav")
        ]))]
    );
    // A double click hears it, on a folder opens it.
    let (a, _) = press(
        &mut view,
        &s,
        Point::new(r.x + 60.0, r.y + 10.0),
        PointerButton::Primary,
        2,
        Modifiers::NONE,
    );
    assert_eq!(
        a,
        vec![Action::Browser(BrowserOp::Preview(media.join("Keys.wav")))]
    );
    let d = view.row_rect(row_of(&s, "Drums"), SIZE, &s);
    let (a, _) = press(
        &mut view,
        &s,
        Point::new(d.x + 60.0, d.y + 10.0),
        PointerButton::Primary,
        2,
        Modifiers::NONE,
    );
    assert_eq!(
        a,
        vec![Action::Browser(BrowserOp::Enter(media.join("Drums")))]
    );
    // The third star rates it.
    let star = BrowserView::star_rect(r, 3);
    let (a, _) = press(
        &mut view,
        &s,
        star.center(),
        PointerButton::Primary,
        1,
        Modifiers::NONE,
    );
    assert_eq!(
        a,
        vec![Action::Browser(BrowserOp::Rate {
            paths: vec![media.join("Keys.wav")],
            stars: 3
        })]
    );
    // Column headers sort; chips and places open menus.
    let buttons = BrowserView::buttons(SIZE, &s);
    let at = |h: Hit| buttons.iter().find(|(x, _)| *x == h).unwrap().1.center();
    let (a, _) = press(
        &mut view,
        &s,
        at(Hit::Column(SortBy::Length)),
        PointerButton::Primary,
        1,
        Modifiers::NONE,
    );
    assert_eq!(a, vec![Action::Browser(BrowserOp::Sort(SortBy::Length))]);
    for hit in [Hit::Chip(Chip::Kind), Hit::Chip(Chip::Key), Hit::Place] {
        let (a, req) = press(
            &mut view,
            &s,
            at(hit),
            PointerButton::Primary,
            1,
            Modifiers::NONE,
        );
        assert!(a.is_empty());
        assert!(
            matches!(&req[..], [HostRequest::GrabFocus, HostRequest::ContextMenu { items, .. }] if !items.is_empty()),
            "{hit:?}"
        );
    }
    // The preview's toggles change its settings.
    let (a, _) = press(
        &mut view,
        &s,
        at(Hit::KeyMatch),
        PointerButton::Primary,
        1,
        Modifiers::NONE,
    );
    let mut want = s.browser().settings();
    want.key = !want.key;
    assert_eq!(a, vec![Action::Browser(BrowserOp::Settings(want))]);
}

#[test]
fn a_drag_carries_the_files_out() {
    let (s, media) = session("drag");
    let mut view = BrowserView::new(Theme::default());
    let r = view.row_rect(row_of(&s, "Bass Hit.wav"), SIZE, &s);
    let start = Point::new(r.x + 60.0, r.y + 10.0);
    press(
        &mut view,
        &s,
        start,
        PointerButton::Primary,
        1,
        Modifiers::NONE,
    );
    assert_eq!(view.drag_payload(&s), None, "not before it moves");
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerMove {
            pos: Point::new(start.x - 200.0, start.y + 30.0),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        SIZE,
        &s,
        &mut cx,
    );
    let payload = view.drag_payload(&s).unwrap();
    assert_eq!(
        browser::parse_payload(&payload),
        Some(vec![media.join("Bass Hit.wav")])
    );
    view.event(
        &ViewEvent::PointerUp {
            pos: start,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        SIZE,
        &s,
        &mut cx,
    );
    assert_eq!(view.drag_payload(&s), None);
}

#[test]
fn typing_searches_and_the_arrows_select() {
    let (mut s, media) = session("keys");
    let mut view = BrowserView::new(Theme::default());
    let key = |view: &mut BrowserView, s: &Session, k: KeyCode| {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(
            &ViewEvent::Key {
                key: k,
                modifiers: Modifiers::NONE,
            },
            SIZE,
            s,
            &mut cx,
        );
        actions
    };
    assert_eq!(
        key(&mut view, &s, KeyCode::Down),
        vec![Action::Browser(BrowserOp::Select(vec![
            media.join("Bass Hit.wav")
        ]))]
    );
    assert_eq!(
        key(&mut view, &s, KeyCode::Char('k')),
        vec![Action::Browser(BrowserOp::Search("k".into()))]
    );
    // The search narrows the list (names and tags).
    s.dispatch(Action::Browser(BrowserOp::Search("keys".into())))
        .unwrap();
    assert!(s.wait_for_browser(Duration::from_secs(60)));
    let names: Vec<&str> = s.browser().rows().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["Keys.wav"]);
    assert_eq!(
        key(&mut view, &s, KeyCode::Backspace),
        vec![Action::Browser(BrowserOp::Search("key".into()))]
    );
}

#[test]
fn the_row_menu_finds_similar_sounds_tags_and_places() {
    let (s, media) = session("menu");
    let i = row_of(&s, "Keys.wav");
    let items = BrowserView::row_menu(i, &s);
    let labels: Vec<&str> = items.iter().map(|m| m.label.as_str()).collect();
    for l in [
        "Hear",
        "Find Similar Sounds",
        "Place at the Playhead",
        "Tags",
        "Rating",
        "Read and Analyse Again",
    ] {
        assert!(labels.contains(&l), "{l} in {labels:?}");
    }
    let find = items
        .iter()
        .find(|m| m.label == "Find Similar Sounds")
        .unwrap();
    assert_eq!(
        find.action,
        Some(Action::Browser(BrowserOp::FindSimilar(
            media.join("Keys.wav")
        )))
    );
    let tags = items.iter().find(|m| m.label == "Tags").unwrap();
    let new = tags
        .children
        .iter()
        .find(|m| m.label == "New Tag…")
        .unwrap();
    assert_eq!(
        new.action,
        Some(Action::Browser(BrowserOp::AskTag(vec![
            media.join("Keys.wav")
        ])))
    );
    // Accessible: the toolbar's toggles and every row.
    let view = BrowserView::new(Theme::default());
    let nodes = view.accessible(SIZE, &s);
    let list = nodes.iter().find(|n| n.role == AccessRole::List).unwrap();
    assert_eq!(list.children.len(), s.browser().rows().len());
    assert!(
        list.children
            .iter()
            .any(|n| n.label.starts_with("Keys.wav, 1.00 s"))
    );
    assert!(nodes.iter().any(|n| n.label == "In the Project's Tempo"));
}
