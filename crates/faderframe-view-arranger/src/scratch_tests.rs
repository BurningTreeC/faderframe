//! The arrangement switcher (issue #13): it names what is shown, its menu
//! shows the song or a pad, makes and renames pads; the section and clip
//! menus copy into pads and bring material back (a replacement of another
//! length offered both ways); each arrangement keeps its own place.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::scratch::{PadMaterial, ScratchOp, SongPlacement};
use faderframe_ui_canvas::{AccessRole, RecordingPainter};

const SIZE: Size = Size::new(1400.0, 700.0);

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn press(
    view: &mut ArrangerView,
    s: &Session,
    pos: Point,
    clicks: u32,
) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks,
        },
        SIZE,
        s,
        &mut cx,
    );
    assert!(actions.is_empty(), "a press only asks the host");
    requests
}

/// A press on the switcher.
fn switcher(view: &mut ArrangerView, s: &Session, clicks: u32) -> Vec<HostRequest<Action>> {
    let at = view.arrangement_rect(s).center();
    press(view, s, at, clicks)
}

fn menu(requests: Vec<HostRequest<Action>>) -> Vec<MenuItem<Action>> {
    requests
        .into_iter()
        .find_map(|r| match r {
            HostRequest::ContextMenu { items, .. } => Some(items),
            _ => None,
        })
        .expect("a menu")
}

fn find<'a>(items: &'a [MenuItem<Action>], label: &str) -> &'a MenuItem<Action> {
    items
        .iter()
        .find(|i| i.label == label)
        .unwrap_or_else(|| panic!("no '{label}' in {:?}", labels(items)))
}

fn labels(items: &[MenuItem<Action>]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

fn texts(view: &mut ArrangerView, s: &Session) -> Vec<String> {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p.texts().into_iter().map(str::to_string).collect()
}

#[test]
fn the_switcher_names_what_is_shown_and_switches() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    assert!(texts(&mut view, &s).iter().any(|t| t == "Song"));
    let r = view.arrangement_rect(&s);
    assert_eq!(view.hit_test(r.center(), SIZE, &s), Some(Hit::Arrangement));
    assert_eq!(
        view.hit_test(Point::new(r.right() + 30.0, r.center().y), SIZE, &s),
        Some(Hit::Corner),
        "the grid settings stay beside it"
    );
    let items = menu(press(&mut view, &s, r.center(), 1));
    assert_eq!(find(&items, "Song").checked, Some(true));
    assert!(
        find(&items, "New Scratch Pad from Selection")
            .action
            .is_none()
    );
    // A pad from the demo's verse, from the menu.
    let from_section = find(&items, "New Scratch Pad from Section");
    let verse = find(&from_section.children, "Verse")
        .action
        .clone()
        .unwrap();
    s.dispatch(verse).unwrap();
    assert_eq!(s.shown_arrangement_name(), "Verse Idea");
    assert!(texts(&mut view, &s).iter().any(|t| t == "Verse Idea"));
    let items = menu(switcher(&mut view, &s, 1));
    assert_eq!(find(&items, "Song").checked, Some(false));
    assert_eq!(find(&items, "Verse Idea").checked, Some(true));
    for label in [
        "Rename ‘Verse Idea’…",
        "Duplicate ‘Verse Idea’",
        "Delete ‘Verse Idea’",
    ] {
        assert!(find(&items, label).action.is_some());
    }
    let back = find(&items, "Bring ‘Verse Idea’ to Song");
    assert!(
        back.children
            .iter()
            .any(|i| i.label.starts_with("Insert at Bar"))
    );
    // Double-clicking the pad's name renames it there.
    let req = switcher(&mut view, &s, 2);
    let Some(HostRequest::TextInput {
        initial, commit, ..
    }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::TextInput { .. }))
    else {
        panic!("no text input")
    };
    assert_eq!(initial, "Verse Idea");
    s.dispatch(commit("Verse B").unwrap()).unwrap();
    assert_eq!(s.shown_arrangement_name(), "Verse B");
    // Back to the song.
    let items = menu(switcher(&mut view, &s, 1));
    s.dispatch(find(&items, "Song").action.clone().unwrap())
        .unwrap();
    assert_eq!(s.shown_pad(), None);
    // From the song, each pad has its own entries.
    let items = menu(switcher(&mut view, &s, 1));
    let pads = find(&items, "Scratch Pads");
    let ours = find(&pads.children, "Verse B");
    assert_eq!(
        labels(&ours.children),
        ["Show", "Rename…", "Duplicate", "Delete", "Bring to Song"]
    );
}

#[test]
fn the_ruler_is_tinted_while_a_pad_is_shown() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    // The accent line under the ruler, the whole width.
    let lines = |view: &mut ArrangerView, s: &Session| {
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, s, &Theme::default());
        let y = view.base_ruler_h() - 2.0;
        p.ops
            .iter()
            .filter(|op| {
                matches!(op, faderframe_ui_canvas::DrawOp::Rect(r)
                    if r.y == y && r.h == 2.0 && r.x == 0.0 && r.w == SIZE.w)
            })
            .count()
    };
    assert_eq!(lines(&mut view, &s), 0);
    s.dispatch(Action::ScratchPad(ScratchOp::New {
        name: None,
        from: None,
    }))
    .unwrap();
    assert_eq!(lines(&mut view, &s), 1);
}

#[test]
fn each_arrangement_keeps_its_place() {
    let mut s = session();
    let mut view = ArrangerView::new(Theme::default());
    texts(&mut view, &s);
    view.scroll_x = 900.0;
    view.ppq = 60.0;
    texts(&mut view, &s);
    s.dispatch(Action::ScratchPad(ScratchOp::New {
        name: None,
        from: None,
    }))
    .unwrap();
    texts(&mut view, &s);
    assert_eq!(view.scroll_x, 0.0, "a new pad from its start");
    assert_eq!(view.ppq, 60.0, "at the zoom it was");
    view.scroll_x = 120.0;
    texts(&mut view, &s);
    s.dispatch(Action::ScratchPad(ScratchOp::Show(None)))
        .unwrap();
    texts(&mut view, &s);
    assert_eq!(view.scroll_x, 900.0);
    let pad = s.scratch_pads()[0].id;
    s.dispatch(Action::ScratchPad(ScratchOp::Show(Some(pad))))
        .unwrap();
    texts(&mut view, &s);
    assert_eq!(view.scroll_x, 120.0);
}

#[test]
fn material_menus_copy_into_pads_and_bring_it_back_both_ways() {
    let mut s = session();
    let sections = s.project().sections.clone();
    // In the song: copy a section to a new pad or one there is.
    let items = scratch::material_items(&s, PadMaterial::Section(sections[0].id));
    let copy = find(&items, "Copy to Scratch Pad");
    assert_eq!(labels(&copy.children), ["New Scratch Pad"]);
    assert!(items.iter().all(|i| i.label != "Bring to Song"));
    s.dispatch(copy.children[0].action.clone().unwrap())
        .unwrap();
    let pad = s.shown_pad().unwrap();
    // Shorten the pad's material to two bars: a replacement of a
    // four-bar section is then ambiguous and offered both ways.
    let p = s.project();
    let two_bars = MusicalTime::from_quarters(8.0);
    let mut cut = p.sections[0].clone();
    cut.end = cut.start + two_bars;
    let long: Vec<ClipId> = p.clips.keys().copied().collect();
    s.dispatch(Action::Edit(Command::UpdateSection { section: cut }))
        .unwrap();
    for clip in long {
        s.dispatch(Action::Edit(Command::RemoveClip { clip }))
            .unwrap();
    }
    s.dispatch(Action::CreateMidiClip {
        track: s
            .project()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Instrument)
            .unwrap()
            .id,
        start: MusicalTime::ZERO,
        length: two_bars,
    })
    .unwrap();
    assert_eq!(
        s.pad_material_length(pad, &PadMaterial::All),
        Some(two_bars)
    );
    let items = scratch::to_song_items(&s, pad, PadMaterial::All);
    assert_eq!(
        labels(&items[..2]),
        [
            "Insert at Bar 1 (What Follows Moves Later)",
            "Overwrite from Bar 1 (Time Stays)"
        ]
    );
    let replace = find(&items, "Replace Section");
    let verse = find(&replace.children, "Verse (4 bars; the Material Is 2 bars)");
    let choices: Vec<_> = verse
        .children
        .iter()
        .map(|i| (i.label.as_str(), i.action.clone()))
        .collect();
    assert_eq!(
        choices,
        [
            (
                "Take the Material's Length (What Follows Moves 2 bars Earlier)",
                Some(Action::ScratchPad(ScratchOp::ToSong {
                    pad,
                    what: PadMaterial::All,
                    place: SongPlacement::Replace {
                        section: sections[1].id,
                        keep_length: false
                    }
                }))
            ),
            (
                "Keep the Section's Length (Pad the Material to 4 bars)",
                Some(Action::ScratchPad(ScratchOp::ToSong {
                    pad,
                    what: PadMaterial::All,
                    place: SongPlacement::Replace {
                        section: sections[1].id,
                        keep_length: true
                    }
                }))
            ),
        ]
    );
    // In the pad, its clips' menu brings them back too.
    let clip = s.project().clips.keys().next().copied().unwrap();
    let items = scratch::material_items(&s, PadMaterial::Clips(vec![clip]));
    assert!(find(&items, "Bring to Song").children.len() >= 3);
}

#[test]
fn the_switcher_is_a_tab_list_for_screen_readers() {
    let mut s = session();
    s.dispatch(Action::ScratchPad(ScratchOp::New {
        name: Some("Idea".into()),
        from: None,
    }))
    .unwrap();
    let mut view = ArrangerView::new(Theme::default());
    texts(&mut view, &s);
    let nodes = view.accessible(SIZE, &s);
    let tabs = nodes.last().unwrap();
    assert_eq!(tabs.role, AccessRole::TabList);
    let names: Vec<(&str, Option<bool>)> = tabs
        .children
        .iter()
        .map(|t| (t.label.as_str(), t.selected))
        .collect();
    assert_eq!(names, [("Song", Some(false)), ("Idea", Some(true))]);
    s.dispatch(tabs.children[0].activate.clone().unwrap())
        .unwrap();
    assert_eq!(s.shown_pad(), None);
}
