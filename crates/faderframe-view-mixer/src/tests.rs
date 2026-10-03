use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn run(
    view: &mut MixerView,
    ev: ViewEvent,
    size: Size,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, size, s, &mut cx);
    (actions, requests)
}

fn down(pos: Point, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

#[test]
fn only_visible_strips_are_painted() {
    let mut s = session();
    for _ in 0..60 {
        s.add_track(TrackKind::Audio).unwrap();
    }
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(800.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let names: Vec<&str> = p.texts();
    let visible = view.visible_range(MixerView::channel_tracks(&s).len(), size);
    assert!(visible.len() <= 8, "virtualised: {} strips", visible.len());
    // Strips far outside the viewport are never painted.
    let channels = MixerView::channel_tracks(&s);
    for (i, t) in channels.iter().enumerate() {
        let painted = names.contains(&t.name.as_str());
        if !visible.contains(&i) && t.name.starts_with("Audio ") {
            assert!(!painted, "off-screen strip {i} ({}) was painted", t.name);
        }
    }
    assert!(names.contains(&channels[visible.start].name.as_str()));
    assert!(names.contains(&"MASTER"));
    assert!(p.balanced_clips());
}

#[test]
fn mute_button_toggles_via_command() {
    let s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 760.0);
    let first = MixerView::channel_tracks(&s)[0];
    let l = view.layout_for(view.strip_rect(0, size), first);
    let (actions, _) = run(&mut view, down(l.mute.center(), 1), size, &s);
    assert_eq!(
        actions,
        vec![Action::Edit(Command::SetTrackMute {
            track: first.id,
            on: true
        })]
    );
}

#[test]
fn fader_drag_is_one_gesture_and_double_click_resets() {
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 760.0);
    let bass = MixerView::channel_tracks(&s)[1].id;
    let l = view.layout_of(&s, bass, size).unwrap();
    let geo = FaderGeometry::new(l.fader, &theme);
    let t = s.project().track(bass).unwrap();
    let cap = geo.cap_rect(view.law.db_to_position(t.volume_db)).center();

    let (a, _) = run(&mut view, down(cap, 1), size, &s);
    assert_eq!(a, vec![Action::BeginGesture("Volume".into())]);
    let mut all = a;
    for dy in [10.0, 20.0, 40.0] {
        let (a, _) = run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(cap.x, cap.y - dy),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &s,
        );
        all.extend(a);
    }
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerUp {
            pos: cap,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    all.extend(a);
    assert_eq!(all.last(), Some(&Action::EndGesture));
    for a in all {
        s.dispatch(a).unwrap();
    }
    let after = s.project().track(bass).unwrap().volume_db;
    assert!(after > -3.0, "moved up: {after}");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().track(bass).unwrap().volume_db,
        -3.0,
        "one undo step"
    );

    let (a, _) = run(&mut view, down(cap, 2), size, &s);
    assert_eq!(
        a,
        vec![Action::Edit(Command::SetTrackVolume {
            track: bass,
            db: 0.0
        })]
    );
}

#[test]
fn routing_and_insert_clicks_open_menus() {
    let s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme);
    let size = Size::new(1400.0, 760.0);
    let first = MixerView::channel_tracks(&s)[0];
    let l = view.layout_of(&s, first.id, size).unwrap();
    let (_, req) = run(&mut view, down(l.output.center(), 1), size, &s);
    match req.first() {
        Some(HostRequest::ContextMenu { items, .. }) => {
            assert!(items.iter().any(|i| i.label == "Master"));
            assert!(items.iter().any(|i| i.label.starts_with("Drum Bus")));
            assert!(
                !items.iter().any(|i| i.label.starts_with("Drums")),
                "cannot route to itself"
            );
        }
        _ => panic!("expected output menu"),
    }
    // An empty insert slot opens the plugin browser for that slot …
    let slot = l.inserts.unwrap()[0];
    let (a, _) = run(&mut view, down(slot.center(), 1), size, &s);
    assert_eq!(
        a,
        vec![Action::OpenPluginBrowser {
            track: first.id,
            target: faderframe_session::PluginTarget::Insert(0),
        }]
    );
    // … and right-click gives the quick list, browser first.
    let ev = ViewEvent::PointerDown {
        pos: slot.center(),
        button: PointerButton::Secondary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    };
    let (_, req) = run(&mut view, ev, size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("expected the insert menu");
    };
    assert_eq!(items[0].label, "Browse Plugins…");
    assert!(items.iter().any(|i| i.label.contains("Echo")));
}

#[test]
fn numeric_entry_parses_levels() {
    assert_eq!(parse_db("-3.5"), Some(-3.5));
    assert_eq!(parse_db(" 6 dB"), Some(6.0));
    assert_eq!(parse_db("-inf"), Some(SILENCE_DB));
    assert_eq!(parse_db("loud"), None);
}

#[test]
fn many_sends_grow_the_send_section_and_page_in_banks() {
    let mut s = session();
    let theme = Theme::default();
    let size = Size::new(1400.0, 1000.0);
    let source = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap()
        .id;
    let mut auxes = Vec::new();
    for _ in 0..9 {
        auxes.push(s.add_track(TrackKind::Aux).unwrap());
    }
    // Five sends on one strip: three rows (5 sends + 1 free slot).
    let existing = s.project().track(source).unwrap().sends.len();
    for aux in auxes.iter().take(5usize.saturating_sub(existing)) {
        s.dispatch(Action::AddSend {
            track: source,
            target: *aux,
            level_db: -6.0,
            tap: SendTap::PostFader,
        })
        .unwrap();
    }
    let mut view = MixerView::new(theme.clone());
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    assert_eq!(view.send_rows, 3);
    let i = MixerView::channel_tracks(&s)
        .iter()
        .position(|t| t.id == source)
        .unwrap();
    let l = view.layout_for(view.strip_rect(i, size), s.project().track(source).unwrap());
    let slots = l.sends.clone().unwrap();
    assert_eq!(slots.len(), 6);
    assert_eq!(
        view.hit_test(slots[4].knob.center(), size, &s),
        Some(Hit::Send(source, 4))
    );
    // The free sixth slot offers the remaining auxes, not the used ones.
    let (_, req) = run(&mut view, down(slots[5].knob.center(), 1), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("add-send menu expected");
    };
    let used: Vec<String> = s
        .project()
        .track(source)
        .unwrap()
        .sends
        .iter()
        .map(|x| format!("Send to {}", s.project().track(x.target).unwrap().name))
        .collect();
    assert!(items.iter().all(|it| !used.contains(&it.label)));
    assert!(!items.is_empty());

    // Nine sends need more than one bank of eight.
    for aux in &auxes[5..] {
        s.dispatch(Action::AddSend {
            track: source,
            target: *aux,
            level_db: -6.0,
            tap: SendTap::PostFader,
        })
        .unwrap();
    }
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    assert_eq!(view.send_rows, MAX_SEND_ROWS);
    let l = view.layout_for(view.strip_rect(i, size), s.project().track(source).unwrap());
    let next = l.send_next.unwrap();
    assert_eq!(
        view.hit_test(next.center(), size, &s),
        Some(Hit::SendBank(1))
    );
    run(&mut view, down(next.center(), 1), size, &s);
    assert_eq!(view.send_bank, 1);
    let slots = l.sends.unwrap();
    assert_eq!(
        view.hit_test(slots[0].knob.center(), size, &s),
        Some(Hit::Send(source, 8))
    );
}
