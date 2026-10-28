#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

fn session_with_reference() -> (Session, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("ff-view-reference-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Record.wav");
    let x: Vec<f32> = (0..48_000 * 3)
        .map(|i| (i as f32 * 0.05).sin() * 0.3)
        .collect();
    faderframe_audio_files::write_wav(
        &file,
        &[x.clone(), x],
        48_000,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut s = Session::new(
        faderframe_project::Project::new("Refs", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    s.dispatch(Action::Reference(ReferenceOp::Add(vec![file])))
        .unwrap();
    assert!(s.wait_for_compare(std::time::Duration::from_secs(120)));
    (s, dir)
}

fn click(
    view: &mut ReferenceView,
    s: &Session,
    pos: Point,
    button: PointerButton,
) -> (Vec<Action>, usize) {
    let size = Size::new(1000.0, 400.0);
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerDown {
            pos,
            button,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        size,
        s,
        &mut cx,
    );
    (actions, requests.len())
}

#[test]
fn the_view_lists_references_and_flips_between_mix_and_reference() {
    let (s, dir) = session_with_reference();
    let id = s.references().tracks[0].id;
    assert_eq!(s.current_reference(), Some(id));
    let mut view = ReferenceView::new(Theme::default());
    let size = Size::new(1000.0, 400.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.balanced_clips());
    let texts = p.texts();
    for t in [
        "Reference",
        "Mix",
        "Record",
        "Along the Song ▾",
        "☑ Match",
        "Send to EQ Match ▾",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    assert!(
        texts.iter().any(|t| t.starts_with("Hearing: Mix")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("Reference -")), "{texts:?}");

    // The Reference half of the switch: heard.
    let button = |hit| {
        ReferenceView::buttons(size)
            .into_iter()
            .find(|(h, _)| *h == hit)
            .unwrap()
            .1
            .center()
    };
    let (actions, _) = click(
        &mut view,
        &s,
        button(Hit::Reference),
        PointerButton::Primary,
    );
    assert_eq!(actions, [Action::Compare(CompareOp::Hear(true))]);
    let (actions, _) = click(&mut view, &s, button(Hit::Mix), PointerButton::Primary);
    assert_eq!(actions, [Action::Compare(CompareOp::Hear(false))]);
    let (actions, _) = click(&mut view, &s, button(Hit::Match), PointerButton::Primary);
    assert_eq!(
        actions,
        [Action::Reference(ReferenceOp::SetMatch(id, false))]
    );
    // Add, Sync and Send ask the host (a chooser, menus).
    for hit in [Hit::Add, Hit::Sync, Hit::SendToEq] {
        let (actions, requests) = click(&mut view, &s, button(hit), PointerButton::Primary);
        assert!(actions.is_empty() && requests == 1, "{hit:?}");
    }
    // A row's menu.
    let row = ReferenceView::row_rect(0, size).center();
    let (_, requests) = click(&mut view, &s, row, PointerButton::Secondary);
    assert_eq!(requests, 1);
    // Files dropped are added.
    let dropped = view.drop_files(&[dir.join("Other.wav")], row, size, &s);
    assert!(matches!(
        dropped,
        Some(Action::Reference(ReferenceOp::Add(_)))
    ));
    // A region dragged on the waveform.
    let wave = ReferenceView::wave_rect(size);
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    for ev in [
        ViewEvent::PointerDown {
            pos: Point::new(wave.x + wave.w * 0.25, wave.center().y),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        ViewEvent::PointerMove {
            pos: Point::new(wave.x + wave.w * 0.5, wave.center().y),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        ViewEvent::PointerUp {
            pos: Point::new(wave.x + wave.w * 0.5, wave.center().y),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
    ] {
        view.event(&ev, size, &s, &mut cx);
    }
    let frames = s.reference_frames(id).unwrap();
    match &actions[..] {
        [Action::Reference(ReferenceOp::SetRegion(r, Some((a, b))))] => {
            assert_eq!(*r, id);
            assert!((*a - frames / 4).abs() < frames / 100, "{a}");
            assert!((*b - frames / 2).abs() < frames / 100, "{b}");
        }
        other => panic!("{other:?}"),
    }
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_view_asks_for_references() {
    let s = Session::new(
        faderframe_project::Project::new("Empty", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let mut view = ReferenceView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, Size::new(800.0, 300.0), &s, &Theme::default());
    let texts = p.texts();
    assert!(texts.contains(&"No references yet: + Add…, or drop files here"));
    assert!(texts.contains(&"Add a finished record to compare the mix with"));
}
