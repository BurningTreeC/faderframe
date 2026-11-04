#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, History};
use faderframe_session::bisect::BisectStage;
use faderframe_ui_canvas::{DrawOp, Modifiers, RecordingPainter};

fn bass(s: &Session) -> faderframe_core::TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id
}

fn click(
    view: &mut HistoryView,
    pos: Point,
    button: PointerButton,
    size: Size,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let (mut actions, mut requests) = (Vec::new(), Vec::new());
    view.event(
        &ViewEvent::PointerDown {
            pos,
            button,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        size,
        s,
        &mut EventCx::new(&mut actions, &mut requests),
    );
    (actions, requests)
}

fn painted(view: &mut HistoryView, size: Size, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, s, &Theme::default());
    p
}

fn click_row(
    view: &mut HistoryView,
    i: usize,
    button: PointerButton,
    size: Size,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let pos = view.row_rect(i, size, s).center();
    click(view, pos, button, size, s)
}

/// Where `text` was drawn.
fn text_rect(p: &RecordingPainter, text: &str) -> Option<Rect> {
    p.ops.iter().find_map(|o| match o {
        DrawOp::Text(t, r) if t == text => Some(*r),
        _ => None,
    })
}

#[test]
fn the_history_lists_the_steps_and_a_click_goes_to_one() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = bass(&s);
    for (i, db) in [-4.0, -5.0, -6.0].into_iter().enumerate() {
        s.dispatch(Action::Edit(Command::SetTrackVolume { track: bass, db }))
            .unwrap();
        s.dispatch(Action::Edit(Command::RenameTrack {
            track: bass,
            name: format!("Bass {i}"),
        }))
        .unwrap();
    }
    s.dispatch(Action::Undo).unwrap();
    let mut view = HistoryView::new(Theme::default());
    let size = Size::new(400.0, 600.0);
    let texts = painted(&mut view, size, &s).texts().join("|");
    assert!(texts.contains("Project opened"));
    assert!(texts.contains("5 steps done · 1 to redo · click one to go to it"));
    // A click on the second step: the project as just after it.
    let second = s.history_tree()[2].step.id;
    let row = view.row_rect(2, size, &s);
    let (mut actions, _) = click(&mut view, row.center(), PointerButton::Primary, size, &s);
    assert_eq!(actions, [Action::HistoryToNode(second)]);
    s.dispatch(actions.remove(0)).unwrap();
    let t = s.project().track(bass).unwrap();
    assert_eq!((t.volume_db, t.name.as_str()), (-4.0, "Bass 0"));
    assert_eq!(s.history_steps().0.len(), 2);
    assert_eq!(s.history_steps().1.len(), 4);
    // Forward again to the last step, and all the way back.
    s.dispatch(Action::HistoryTo(6)).unwrap();
    assert_eq!(s.project().track(bass).unwrap().name, "Bass 2");
    s.dispatch(Action::HistoryTo(0)).unwrap();
    assert_eq!(s.project().track(bass).unwrap().name, "Bass");
}

#[test]
fn a_branch_is_shown_one_level_in_after_the_step_it_starts_from() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = bass(&s);
    for db in [-4.0, -5.0, -6.0] {
        s.dispatch(Action::Edit(Command::SetTrackVolume { track: bass, db }))
            .unwrap();
    }
    s.dispatch(Action::HistoryTo(1)).unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: bass,
        name: "Low".into(),
    }))
    .unwrap();
    let mut view = HistoryView::new(Theme::default());
    let size = Size::new(420.0, 500.0);
    let p = painted(&mut view, size, &s);
    let texts = p.texts();
    assert!(
        texts.contains(&"4 steps · 2 branches · 2 done · click one to go to it"),
        "{texts:?}"
    );
    assert!(texts.contains(&"branch 2"));
    // The branch's step sits one level in, right after the step it was
    // made from; the first line goes on after it.
    let rows = s.history_tree();
    let labels: Vec<&str> = rows.iter().map(|r| r.step.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Project opened",
            "Change Volume",
            "Rename Track",
            "Change Volume",
            "Change Volume"
        ]
    );
    let rename = text_rect(&p, "Rename Track").unwrap();
    let first = view.row_rect(1, size, &s);
    let label_x = |r: Rect| {
        p.ops
            .iter()
            .find_map(|o| match o {
                DrawOp::Text(_, t) if t.y == r.y && t.x > r.x + 40.0 => Some(t.x),
                _ => None,
            })
            .unwrap()
    };
    assert!((rename.x - label_x(first) - INDENT_W).abs() < 0.5);
    // A click on the first line's end goes over there.
    let end = rows[4].step.id;
    let (actions, _) = click_row(&mut view, 4, PointerButton::Primary, size, &s);
    assert_eq!(actions, [Action::HistoryToNode(end)]);
    // Its menu: go, audition, Audio Bisect.
    let (_, requests) = click_row(&mut view, 1, PointerButton::Secondary, size, &s);
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!("a menu");
    };
    let names: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(names.contains(&"Audition: Compare with Now (A/B)"));
    let since = items
        .iter()
        .find(|i| i.label.starts_with("Audio Bisect: Find"))
        .unwrap();
    assert_eq!(
        since.action,
        Some(Action::Bisect(BisectOp::Start {
            good: rows[1].step.id,
            bad: None
        }))
    );
}

#[test]
fn a_step_is_auditioned_beside_the_mix_and_flipped() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = bass(&s);
    // One bar looped: a short audition.
    let meter = s.project().timeline.meter.clone();
    let range = faderframe_project::MusicalRange::new(meter.bar_start(2), meter.bar_start(3));
    s.dispatch(Action::Edit(Command::SetLoop {
        range,
        enabled: true,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: bass,
        db: -20.0,
    }))
    .unwrap();
    let mut view = HistoryView::new(Theme::default());
    let size = Size::new(500.0, 400.0);
    let (mut actions, mut requests) = (Vec::new(), Vec::new());
    // The row's menu offers it; hovering shows its A/B button.
    let row = view.row_rect(1, size, &s);
    let (_, menu) = click(&mut view, row.center(), PointerButton::Secondary, size, &s);
    assert_eq!(menu.len(), 1);
    let button = HistoryView::audition_button(row).center();
    view.event(
        &ViewEvent::PointerMove {
            pos: button,
            modifiers: Modifiers::NONE,
            dragging: false,
        },
        size,
        &s,
        &mut EventCx::new(&mut actions, &mut requests),
    );
    assert!(painted(&mut view, size, &s).texts().contains(&"A/B"));
    let (mut actions, _) = click(&mut view, button, PointerButton::Primary, size, &s);
    let first = s.history_tree()[1].step.id;
    assert_eq!(actions, [Action::AuditionNode(first)]);
    s.dispatch(actions.remove(0)).unwrap();
    assert!(s.wait_for_compare(std::time::Duration::from_secs(300)));
    // The bar: Now, Then, the match, what is heard.
    let p = painted(&mut view, size, &s);
    let texts = p.texts();
    for t in ["Now", "Then", "☑ Match Loudness", "◆ Then"] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    assert!(
        texts.iter().any(|t| t.starts_with("Hearing: Now")),
        "{texts:?}"
    );
    let at = |hit| {
        HistoryView::compare_buttons(size)
            .into_iter()
            .find(|(h, _)| *h == hit)
            .unwrap()
            .1
            .center()
    };
    for (hit, want) in [
        (CompareHit::Then, Action::Compare(CompareOp::Hear(true))),
        (
            CompareHit::Match,
            Action::Compare(CompareOp::MatchLoudness(false)),
        ),
        (CompareHit::Close, Action::Compare(CompareOp::Off)),
    ] {
        let (got, _) = click(&mut view, at(hit), PointerButton::Primary, size, &s);
        assert_eq!(got, std::slice::from_ref(&want));
        s.dispatch(want).unwrap();
    }
    // Ended: the list as before.
    assert!(!painted(&mut view, size, &s).texts().contains(&"Then"));
    assert_eq!(s.project().track(bass).unwrap().volume_db, -20.0);
}

#[test]
fn audio_bisect_is_driven_from_its_bar() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = bass(&s);
    let meter = s.project().timeline.meter.clone();
    let range = faderframe_project::MusicalRange::new(meter.bar_start(2), meter.bar_start(3));
    s.dispatch(Action::Edit(Command::SetLoop {
        range,
        enabled: true,
    }))
    .unwrap();
    for db in [-3.0, -30.0, -31.0] {
        s.dispatch(Action::Edit(Command::SetTrackVolume { track: bass, db }))
            .unwrap();
    }
    let mut view = HistoryView::new(Theme::default());
    let size = Size::new(560.0, 500.0);
    // Getting ready: a click marks the good state, the bar starts.
    s.dispatch(Action::Bisect(BisectOp::Setup)).unwrap();
    let p = painted(&mut view, size, &s);
    assert!(p.texts().contains(&"Audio Bisect"));
    assert!(p.texts().contains(&"BAD"), "the project now: the bad state");
    let (_, buttons) = HistoryView::bisect_buttons(size, &s);
    let start = buttons.iter().find(|b| b.hit == BisectHit::Start).unwrap();
    assert!(!start.enabled, "nothing marked yet");
    let good = s.history_tree()[1].step.id;
    let (mut actions, _) = click_row(&mut view, 1, PointerButton::Primary, size, &s);
    assert_eq!(
        actions,
        [Action::Bisect(BisectOp::Mark {
            node: good,
            good: true
        })]
    );
    s.dispatch(actions.remove(0)).unwrap();
    assert!(painted(&mut view, size, &s).texts().contains(&"GOOD"));
    let (_, buttons) = HistoryView::bisect_buttons(size, &s);
    let start = buttons.iter().find(|b| b.hit == BisectHit::Start).unwrap();
    let (mut actions, _) = click(
        &mut view,
        start.rect.center(),
        PointerButton::Primary,
        size,
        &s,
    );
    assert_eq!(
        actions,
        [Action::Bisect(BisectOp::Start { good, bad: None })]
    );
    s.dispatch(actions.remove(0)).unwrap();
    // Rounds: A and B, the answers; the rows in question marked.
    for _ in 0..8 {
        assert!(s.wait_for_bisect(std::time::Duration::from_secs(300)));
        let st = s.bisect_status().unwrap();
        if st.stage != BisectStage::Comparing {
            break;
        }
        let p = painted(&mut view, size, &s);
        let texts = p.texts();
        for t in [
            "A",
            "B",
            "A Is Better",
            "B Is Better",
            "Can’t Tell",
            "GOOD",
            "BAD",
        ] {
            assert!(texts.contains(&t), "{t} in {texts:?}");
        }
        let (_, buttons) = HistoryView::bisect_buttons(size, &s);
        let better = buttons
            .iter()
            .find(|b| b.hit == BisectHit::BetterB)
            .unwrap();
        assert!(better.enabled);
        let (mut actions, _) = click(
            &mut view,
            better.rect.center(),
            PointerButton::Primary,
            size,
            &s,
        );
        assert_eq!(actions, [Action::Bisect(BisectOp::Answer(BisectAnswer::B))]);
        s.dispatch(actions.remove(0)).unwrap();
    }
    // Found: what it offers.
    let st = s.bisect_status().unwrap();
    assert_eq!(st.stage, BisectStage::Found);
    let p = painted(&mut view, size, &s);
    let texts = p.texts();
    for t in [
        "Hear It (Before/After)",
        "Go Before It",
        "Go There",
        "Close",
        "FOUND",
    ] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    assert!(texts.iter().any(|t| t.starts_with("Found: ")), "{texts:?}");
    let (_, buttons) = HistoryView::bisect_buttons(size, &s);
    let close = buttons.iter().find(|b| b.hit == BisectHit::Cancel).unwrap();
    let (mut actions, _) = click(
        &mut view,
        close.rect.center(),
        PointerButton::Primary,
        size,
        &s,
    );
    assert_eq!(actions, [Action::Bisect(BisectOp::Cancel)]);
    s.dispatch(actions.remove(0)).unwrap();
    assert!(
        !painted(&mut view, size, &s)
            .texts()
            .contains(&"Audio Bisect")
    );
    assert_eq!(s.history_current(), s.history_tree()[4].step.id);
    assert!(s.history_step(History::ROOT).is_some());
}
