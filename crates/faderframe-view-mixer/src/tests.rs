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
    assert!(items.iter().any(|i| i.label.contains("Delay")));
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

fn right(pos: Point) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Secondary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    }
}

fn menu_labels(req: Vec<HostRequest<Action>>) -> Vec<String> {
    match req.into_iter().next() {
        Some(HostRequest::ContextMenu { items, .. }) => {
            items.into_iter().map(|i| i.label).collect()
        }
        _ => panic!("expected a menu"),
    }
}

#[test]
fn faders_knobs_and_buttons_offer_midi_learn() {
    let s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 760.0);
    let first = MixerView::channel_tracks(&s)[0];
    let l = view.layout_of(&s, first.id, size).unwrap();
    for at in [l.fader.center(), l.mute.center()] {
        let (_, req) = run(&mut view, right(at), size, &s);
        let labels = menu_labels(req);
        assert!(labels.iter().any(|l| l == "MIDI Learn…"), "{labels:?}");
    }
    // Learning starts from the menu entry.
    let (_, req) = run(&mut view, right(l.fader.center()), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!()
    };
    let learn = items
        .into_iter()
        .find(|i| i.label == "MIDI Learn…")
        .unwrap();
    assert!(matches!(
        learn.action,
        Some(Action::MidiLearn(
            faderframe_project::MappingTarget::Parameter { .. }
        ))
    ));
}

#[test]
fn instrument_strips_choose_a_midi_input() {
    let s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 760.0);
    let synth = MixerView::channel_tracks(&s)
        .into_iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap();
    let l = view.layout_of(&s, synth.id, size).unwrap();
    let input = l.input.expect("instrument strips show their input").input;
    let (_, req) = run(&mut view, right(input.center()), size, &s);
    let labels = menu_labels(req);
    for want in [
        "All MIDI Inputs",
        "All Channels",
        "Channel 10",
        "Play Live: always",
    ] {
        assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
    }
}

#[test]
fn clicking_the_pan_value_opens_a_text_field() {
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1200.0, 700.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let l = view.layout_of(&s, bass, size).unwrap();
    assert_eq!(
        view.hit_test(l.pan_readout.center(), size, &s),
        Some(Hit::PanValue(bass))
    );
    let (_, requests) = run(&mut view, down(l.pan_readout.center(), 1), size, &s);
    let Some(HostRequest::TextInput {
        initial, commit, ..
    }) = requests.into_iter().next()
    else {
        panic!("a text field")
    };
    assert_eq!(initial, "C");
    let action = commit("L30").expect("valid pan");
    s.dispatch(action).unwrap();
    let t = s.project().track(bass).unwrap();
    assert!((t.pan + 0.3).abs() < 1e-6, "{}", t.pan);
    assert!(commit("nonsense").is_none());
}

#[test]
fn the_inserts_grip_sizes_the_section_and_more_slots_show() {
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 900.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let bass = MixerView::channel_tracks(&s)[1].id;
    let l = view.layout_of(&s, bass, size).unwrap();
    assert_eq!(
        l.inserts.as_ref().map(Vec::len),
        Some(5),
        "five slots by default"
    );
    let grip = l.inserts_grip.unwrap().center();
    assert_eq!(view.hit_test(grip, size, &s), Some(Hit::InsertsGrip(bass)));
    // Drag three slots down.
    let mut actions = run(&mut view, down(grip, 1), size, &s).0;
    let to = Point::new(grip.x, grip.y + 3.0 * INSERT_SLOT_STEP + 2.0);
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: to,
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &s,
        )
        .0,
    );
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerUp {
                pos: to,
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            size,
            &s,
        )
        .0,
    );
    assert_eq!(actions, vec![Action::SetMixerInsertSlots(8)]);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.mixer_insert_slots(), 8);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let l = view.layout_of(&s, bass, size).unwrap();
    assert_eq!(l.inserts.as_ref().map(Vec::len), Some(8));
    // Double-click resets.
    let grip = l.inserts_grip.unwrap().center();
    let (a, _) = run(&mut view, down(grip, 2), size, &s);
    assert_eq!(a, vec![Action::SetMixerInsertSlots(5)]);
    // More plugins than slots: "+N more" grows the section.
    s.dispatch(Action::SetMixerInsertSlots(2)).unwrap();
    for _ in 0..4 {
        let n = s.project().track(bass).unwrap().inserts.len();
        s.dispatch(Action::InsertPlugin {
            track: bass,
            index: n,
            plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::GAIN, "Gain"),
        })
        .unwrap();
    }
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.texts().contains(&"+3 more"), "{:?}", p.texts());
    let l = view.layout_of(&s, bass, size).unwrap();
    let last = l.inserts.as_ref().unwrap()[1].center();
    let (a, _) = run(&mut view, down(last, 1), size, &s);
    assert_eq!(a, vec![Action::SetMixerInsertSlots(5)]);
}

fn press_drag_release(
    view: &mut MixerView,
    s: &Session,
    size: Size,
    from: Point,
    to: Point,
    m: Modifiers,
) -> Vec<Action> {
    let mut all = run(
        view,
        ViewEvent::PointerDown {
            pos: from,
            button: PointerButton::Primary,
            modifiers: m,
            clicks: 1,
        },
        size,
        s,
    )
    .0;
    if to != from {
        for p in [Point::new(from.x + 6.0, from.y + 3.0), to] {
            all.extend(
                run(
                    view,
                    ViewEvent::PointerMove {
                        pos: p,
                        modifiers: m,
                        dragging: true,
                    },
                    size,
                    s,
                )
                .0,
            );
        }
    }
    all.extend(
        run(
            view,
            ViewEvent::PointerUp {
                pos: to,
                button: PointerButton::Primary,
                modifiers: m,
            },
            size,
            s,
        )
        .0,
    );
    all
}

#[test]
fn inserts_drag_to_reorder_copy_and_alt_click_removes() {
    use faderframe_core::builtin;
    use faderframe_project::PluginRef;
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 900.0);
    let channels = MixerView::channel_tracks(&s);
    let (bass, pluck) = (channels[1].id, channels[2].id);
    for (i, (id, name)) in [(builtin::GAIN, "Gain"), (builtin::ECHO, "Echo")]
        .into_iter()
        .enumerate()
    {
        s.dispatch(Action::InsertPlugin {
            track: bass,
            index: i,
            plugin: PluginRef::builtin(id, name),
        })
        .unwrap();
    }
    let names = |s: &Session, t| -> Vec<String> {
        s.project()
            .track(t)
            .unwrap()
            .inserts
            .iter()
            .map(|x| x.plugin.name.clone())
            .collect()
    };
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let slots = |v: &MixerView, s: &Session, t| v.layout_of(s, t, size).unwrap().inserts.unwrap();
    let b = slots(&view, &s, bass);
    let gain = s.project().track(bass).unwrap().inserts[0].id;
    // A click opens the editor; Ctrl-click toggles bypass.
    let a = press_drag_release(
        &mut view,
        &s,
        size,
        b[0].center(),
        b[0].center(),
        Modifiers::NONE,
    );
    assert_eq!(
        a,
        vec![Action::OpenPluginEditor {
            track: bass,
            plugin: gain,
            generic: false
        }]
    );
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let a = press_drag_release(&mut view, &s, size, b[0].center(), b[0].center(), ctrl);
    assert!(matches!(
        a[..],
        [Action::Edit(Command::SetPluginBypass { bypass: true, .. })]
    ));
    // Dragging the first insert onto the second slot reorders, one step.
    let a = press_drag_release(
        &mut view,
        &s,
        size,
        b[0].center(),
        b[1].center(),
        Modifiers::NONE,
    );
    for x in a {
        s.dispatch(x).unwrap();
    }
    assert_eq!(names(&s, bass), ["Echo", "Gain"]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(names(&s, bass), ["Gain", "Echo"]);
    // Set a parameter on the echo, then drag it onto the Pluck strip: a
    // copy with the same settings.
    let echo = s.project().track(bass).unwrap().inserts[1].clone();
    let param = faderframe_core::ParameterId(1);
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: bass,
        plugin: echo.id,
        parameter: param,
        value: Some(0.37),
    }))
    .unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let pl = slots(&view, &s, pluck);
    let a = press_drag_release(
        &mut view,
        &s,
        size,
        b[1].center(),
        pl[0].center(),
        Modifiers::NONE,
    );
    for x in a {
        s.dispatch(x).unwrap();
    }
    let copy = s.project().track(pluck).unwrap().inserts[0].clone();
    assert_eq!(copy.plugin, echo.plugin);
    assert_ne!(copy.id, echo.id, "a new instance");
    assert!(
        copy.parameters
            .iter()
            .any(|p| p.id == param && (p.value - 0.37).abs() < 1e-6),
        "settings travel with the copy: {:?}",
        copy.parameters
    );
    assert_eq!(names(&s, bass), ["Gain", "Echo"], "the original stays");
    // Shift-drag moves to the other track instead.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let a = press_drag_release(&mut view, &s, size, b[0].center(), pl[1].center(), shift);
    for x in a {
        s.dispatch(x).unwrap();
    }
    assert_eq!(names(&s, bass), ["Echo"]);
    assert_eq!(names(&s, pluck), ["Echo", "Gain"]);
    // Alt-click removes.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let b = slots(&view, &s, bass);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    let a = press_drag_release(&mut view, &s, size, b[0].center(), b[0].center(), alt);
    for x in a {
        s.dispatch(x).unwrap();
    }
    assert!(names(&s, bass).is_empty());
}

#[test]
fn dragging_bottom_names_reorders_tracks_once_and_preserves_contents() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let channels = MixerView::channel_tracks(&s);
    let a = channels[0].id;
    let b = channels[1].id;
    let original = s.project().tracks.clone();
    let clips = s.project().clips.clone();
    let from = view.layout_of(&s, a, size).unwrap().scribble.center();
    let to = view.layout_of(&s, b, size).unwrap().scribble.center();
    for action in press_drag_release(&mut view, &s, size, from, to, Modifiers::NONE) {
        s.dispatch(action).unwrap();
    }
    assert_eq!(MixerView::channel_tracks(&s)[0].id, b);
    assert_eq!(MixerView::channel_tracks(&s)[1].id, a);
    assert_eq!(s.project().clips, clips);
    assert_eq!(s.project().track(a), original.iter().find(|t| t.id == a));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks, original);
    let actions = press_drag_release(&mut view, &s, size, from, from, Modifiers::NONE);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Edit(Command::MoveTrack { .. })))
    );
    let (_, req) = run(&mut view, down(from, 2), size, &s);
    assert!(
        req.iter()
            .any(|r| matches!(r, HostRequest::TextInput { .. }))
    );
}

#[test]
fn track_drop_uses_visible_order_with_hidden_midi_tracks_and_a_pinned_master() {
    let mut s = session();
    let hidden = s.add_track(TrackKind::Midi).unwrap();
    s.dispatch(Action::Edit(Command::MoveTrack {
        track: hidden,
        index: 1,
    }))
    .unwrap();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let a = MixerView::channel_tracks(&s)[0].id;
    let b = MixerView::channel_tracks(&s)[1].id;
    let from = view.layout_of(&s, b, size).unwrap().scribble.center();
    let to = Point::new(view.cheek() + 1.0, from.y);
    for action in press_drag_release(&mut view, &s, size, from, to, Modifiers::NONE) {
        s.dispatch(action).unwrap();
    }
    assert_eq!(MixerView::channel_tracks(&s)[0].id, b);
    assert_eq!(MixerView::channel_tracks(&s)[1].id, a);
    assert!(s.project().track(hidden).is_some());
    let master = s.project().master_id().unwrap();
    let from = view.layout_of(&s, master, size).unwrap().scribble.center();
    let actions = press_drag_release(&mut view, &s, size, from, to, Modifiers::NONE);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Edit(Command::MoveTrack { .. })))
    );
}
