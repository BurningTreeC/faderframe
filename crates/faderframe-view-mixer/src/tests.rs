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
    // Laid out by a paint, as before any click in the app.
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let first = MixerView::channel_tracks(&s)[0];
    let l = view.layout_for(view.strip_rect(0, size), first, s.project());
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
    // Laid out by a paint, as before any click in the app.
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
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
    // Laid out by a paint, as before any click in the app.
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
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
fn an_empty_insert_slot_offers_the_favorites_by_category() {
    use faderframe_session::plugin_favorites::{FavoriteKey, FavoriteOp, FavoritePlugin};
    let mut s = session();
    let eq = s
        .available_plugins()
        .into_iter()
        .find(|p| p.plugin.id == faderframe_core::builtin::EQ)
        .unwrap();
    s.dispatch(Action::PluginFavorites(FavoriteOp::NewCategory {
        name: "Mixing".into(),
        with: Some(FavoritePlugin::of(&eq)),
    }))
    .unwrap();
    // A favorite whose plugin is not installed here.
    s.dispatch(Action::PluginFavorites(FavoriteOp::Set {
        plugin: FavoritePlugin {
            key: FavoriteKey {
                format: faderframe_project::PluginFormat::Clap,
                id: "com.example.gone".into(),
            },
            name: "Gone Comp".into(),
            vendor: "Example".into(),
        },
        favorite: true,
    }))
    .unwrap();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 760.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let first = MixerView::channel_tracks(&s)[0];
    let slot = view.layout_of(&s, first.id, size).unwrap().inserts.unwrap()[0];
    let (_, req) = run(&mut view, right(slot.center()), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("expected the insert menu");
    };
    assert_eq!(items[0].label, "Browse Plugins…");
    let favorites = &items[1];
    assert_eq!(favorites.label, "Favorites");
    let labels: Vec<&str> = favorites
        .children
        .iter()
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [eq.plugin.name.as_str(), "Gone Comp (missing)", "Mixing"]
    );
    assert_eq!(
        favorites.children[0].action,
        Some(Action::InsertPlugin {
            track: first.id,
            index: 0,
            plugin: eq.plugin.clone(),
        })
    );
    assert!(favorites.children[1].action.is_none(), "missing: disabled");
    assert_eq!(favorites.children[2].children.len(), 1, "the category's");
    // Without favorites there is no such submenu.
    let s = session();
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let (_, req) = run(&mut view, right(slot.center()), size, &s);
    assert!(!menu_labels(req).iter().any(|l| l == "Favorites"));
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
    let l = view.layout_for(
        view.strip_rect(i, size),
        s.project().track(source).unwrap(),
        s.project(),
    );
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
    let l = view.layout_for(
        view.strip_rect(i, size),
        s.project().track(source).unwrap(),
        s.project(),
    );
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
    // Laid out by a paint, as before any click in the app.
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
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
    // Laid out by a paint, as before any click in the app.
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
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
    // Two slots: the first plugin, and how many more there are.
    let count = s.project().track(bass).unwrap().inserts.len();
    let more = format!("+{} more", count - 1);
    assert!(p.texts().contains(&more.as_str()), "{:?}", p.texts());
    let l = view.layout_of(&s, bass, size).unwrap();
    let last = l.inserts.as_ref().unwrap()[1].center();
    let (a, _) = run(&mut view, down(last, 1), size, &s);
    // Every plugin and an empty slot.
    assert_eq!(a, vec![Action::SetMixerInsertSlots(count as u16 + 1)]);
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
    // The demo, its bass and plucks without the devices they come with.
    let config = EngineConfig::default();
    let mut project = faderframe_project::demo::demo_project(config.sample_rate);
    for t in &mut project.tracks {
        if t.name == "Bass" || t.name == "Pluck" {
            t.inserts.clear();
        }
    }
    let mut s = Session::new(project, None, config).unwrap();
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
            .any(|a| matches!(a, Action::PlaceTrack { .. }))
    );
    let (_, req) = run(&mut view, down(from, 2), size, &s);
    assert!(
        req.iter()
            .any(|r| matches!(r, HostRequest::TextInput { .. }))
    );
}

/// A drop counts where it is across, whatever its height: released below
/// the mixer (or above it) it still moves the strip, past the last strip it
/// goes last, before the first first.
#[test]
fn a_strip_lands_by_where_it_is_across_even_off_the_mixer() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let order =
        |s: &Session| -> Vec<_> { MixerView::channel_tracks(s).iter().map(|t| t.id).collect() };
    let start = order(&s);
    let (a, b) = (start[0], start[1]);
    let from = view.layout_of(&s, a, size).unwrap().scribble.center();
    let to_b = view.layout_of(&s, b, size).unwrap().scribble.center();
    // Released 300 px below the mixer, across from b's middle.
    for action in press_drag_release(
        &mut view,
        &s,
        size,
        from,
        Point::new(to_b.x, size.h + 300.0),
        Modifiers::NONE,
    ) {
        s.dispatch(action).unwrap();
    }
    assert_eq!(&order(&s)[..2], &[b, a]);
    s.dispatch(Action::Undo).unwrap();
    // Past the right end of the strips: last (of the strips in view).
    let last_x = (0..start.len())
        .filter_map(|i| {
            view.layout_of(&s, start[i], size)
                .map(|l| l.scribble.right())
        })
        .fold(0.0f32, f32::max);
    for action in press_drag_release(
        &mut view,
        &s,
        size,
        from,
        Point::new(last_x + 4000.0, -50.0),
        Modifiers::NONE,
    ) {
        s.dispatch(action).unwrap();
    }
    let now = order(&s);
    let visible = (0..start.len())
        .filter(|i| view.layout_of(&s, now[*i], size).is_some())
        .count();
    assert!(
        now.iter().position(|t| *t == a).unwrap() + 1 >= visible,
        "{now:?}"
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
            .any(|a| matches!(a, Action::PlaceTrack { .. }))
    );
}

#[test]
fn preamp_chooser_faceplate_controls_and_removal_use_the_dedicated_slot() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    let track = MixerView::channel_tracks(&s)[0].id;
    view.update_sends(&s);
    let area = view.layout_of(&s, track, size).unwrap().preamp.unwrap();
    let (_, requests) = run(&mut view, down(area.center(), 1), size, &s);
    assert!(matches!(&requests[..], [HostRequest::ContextMenu { items, .. }] if items.len() == 6));
    s.dispatch(Action::SetPreamp {
        track,
        model: Some(0),
    })
    .unwrap();
    view.update_sends(&s);
    let area = view.layout_of(&s, track, size).unwrap().preamp.unwrap();
    assert_eq!(area.h, 84.0);
    let knob = Point::new(area.x + area.w * 0.25, area.y + 30.0);
    assert_eq!(
        view.hit_test(knob, size, &s),
        Some(Hit::PreampKnob(track, 0))
    );
    let (mut actions, _) = run(&mut view, down(knob, 1), size, &s);
    assert_eq!(actions, vec![Action::BeginGesture("Preamp".into())]);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerMove {
            pos: Point::new(knob.x, knob.y - 20.0),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        size,
        &s,
    );
    actions.extend(a);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerUp {
            pos: knob,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    actions.extend(a);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(
        preamp::value(
            s.project().track(track).unwrap().preamp.as_ref().unwrap(),
            0
        ) > 0.5
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        preamp::value(
            s.project().track(track).unwrap().preamp.as_ref().unwrap(),
            0
        ),
        0.5
    );
    let mut painter = RecordingPainter::new();
    view.paint(&mut painter, size, &s, &Theme::default());
    for text in ["British 73", "Gain", "Master", "Change", "×"] {
        assert!(painter.texts().contains(&text), "{text}");
    }
    let remove = Point::new(area.right() - 10.0, area.bottom() - 10.0);
    let (a, _) = run(&mut view, down(remove, 1), size, &s);
    assert_eq!(a, vec![Action::SetPreamp { track, model: None }]);
}

#[test]
fn the_plus_after_the_last_strip_offers_new_tracks() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1800.0, 900.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let count = MixerView::channel_tracks(&s).len();
    let plus = view.add_track_rect(count);
    assert!(plus.x >= view.strip_rect(count - 1, size).right());
    assert_eq!(view.hit_test(plus.center(), size, &s), Some(Hit::AddTrack));
    let (_, req) = run(&mut view, down(plus.center(), 1), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu")
    };
    let instrument = items
        .iter()
        .find(|i| i.label == "Instrument Track…")
        .and_then(|i| i.action.clone())
        .unwrap();
    s.dispatch(instrument).unwrap();
    assert_eq!(MixerView::channel_tracks(&s).len(), count + 1);
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|r| matches!(r, faderframe_session::UiRequest::PluginBrowser { .. }))
    );
}

#[test]
fn strips_are_made_wider_or_narrower_by_their_edge() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1800.0, 900.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let normal = Theme::default().console.strip_width;
    let first = view.strip_rect(0, size);
    let second = view.strip_rect(1, size);
    assert_eq!(first.w, normal);
    let id = MixerView::channel_tracks(&s)[0].id;
    // Drag the first strip's right edge 40 px right: it widens, the next
    // one moves along.
    let edge = Point::new(first.right() + 1.0, 300.0);
    assert_eq!(view.hit_test(edge, size, &s), Some(Hit::Width(id)));
    let (actions, _) = run(&mut view, down(edge, 1), size, &s);
    assert!(actions.is_empty());
    let (actions, _) = run(
        &mut view,
        ViewEvent::PointerMove {
            pos: Point::new(edge.x + 40.0, 300.0),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        size,
        &s,
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.strip_width(id), Some(normal + 40.0));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert_eq!(view.strip_rect(0, size).w, normal + 40.0);
    assert_eq!(view.strip_rect(1, size).x, second.x + 40.0);
    // Every strip at once from the menu; one back to normal.
    let (other, menu) = {
        let t = MixerView::channel_tracks(&s)[1];
        (t.id, MixerView::track_menu(&s, t, Point::new(0.0, 0.0)))
    };
    let HostRequest::ContextMenu { items, .. } = menu else {
        panic!("a menu")
    };
    let narrow = items.iter().find(|i| i.label == "Narrow Strip").unwrap();
    s.dispatch(narrow.action.clone().unwrap()).unwrap();
    assert_eq!(s.strip_width(other), Some(64.0));
    s.dispatch(Action::SetStripWidth {
        track: None,
        width: Some(120.0),
    })
    .unwrap();
    assert_eq!(s.strip_width(id), Some(120.0));
    assert_eq!(s.strip_width(other), Some(120.0));
    // A double-click on an edge: that strip back to the default (the
    // others keep the width set for all).
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let edge = Point::new(view.strip_rect(0, size).right() + 1.0, 300.0);
    let (actions, _) = run(&mut view, down(edge, 2), size, &s);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(s.strip_width(id), Some(120.0), "the width set for all");
    // Narrow strips still lay their controls out inside them.
    s.dispatch(Action::SetStripWidth {
        track: None,
        width: Some(64.0),
    })
    .unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let l = view.layout_of(&s, id, size).unwrap();
    for r in [l.mute, l.solo, l.record, l.pan_knob, l.fader, l.meter] {
        assert!(
            r.x >= l.strip.x && r.right() <= l.strip.right() + 0.01,
            "{r:?} in {:?}",
            l.strip
        );
    }
}

#[test]
fn midi_tracks_have_a_strip_with_their_instrument_and_effects() {
    let mut s = session();
    let lead = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .id;
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    let midi = s.add_track(TrackKind::Midi).unwrap();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1800.0, 900.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let i = MixerView::channel_tracks(&s)
        .iter()
        .position(|t| t.id == midi)
        .unwrap();
    // On its strip: the instrument track it plays ("→ Lead Synth" when its
    // well is compact, the name and its instrument when there is room).
    let strip = view.strip_rect(i, size);
    let texts: Vec<&str> = p
        .ops
        .iter()
        .filter_map(|o| match o {
            faderframe_ui_canvas::DrawOp::Text(t, r) if strip.contains(r.center()) => {
                Some(t.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(
        texts.contains(&"→ Lead Synth") || texts.contains(&"Lead Synth"),
        "{texts:?}"
    );
    for t in ["NOTES", "MIDI OUT —"] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    let l = view.layout_for(
        view.strip_rect(i, size),
        s.project().track(midi).unwrap(),
        s.project(),
    );
    // No fader, pan or sends on it.
    for r in [l.fader, l.pan_knob] {
        assert_eq!(view.hit_test(r.center(), size, &s), Some(Hit::Strip(midi)));
    }
    // The instrument it plays: a menu of instrument tracks.
    let plays = l.preamp.unwrap();
    assert_eq!(
        view.hit_test(plays.center(), size, &s),
        Some(Hit::Plays(midi))
    );
    let (_, req) = run(&mut view, down(plays.center(), 1), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu")
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["None", "Lead Synth (Synth)", "Arp Synth (Synth)"]);
    // The output well: external MIDI devices.
    let (_, req) = run(&mut view, down(l.output.center(), 1), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu")
    };
    assert_eq!(items[0].label, "No External MIDI Output");
    // An empty insert slot opens the browser for a MIDI effect.
    let slot = l.inserts.as_ref().unwrap()[0];
    let (actions, _) = run(&mut view, down(slot.center(), 1), size, &s);
    assert!(actions.contains(&Action::OpenPluginBrowser {
        track: midi,
        target: faderframe_session::PluginTarget::Insert(0),
    }));
}

#[test]
fn sounding_notes_are_named_as_they_fit() {
    let keys = (1u128 << 60) | (1 << 64) | (1 << 67) | (1 << 72);
    assert_eq!(note_names(0, 80.0), "—");
    assert_eq!(note_names(keys, 200.0), "C4 E4 G4 C5");
    assert_eq!(note_names(keys, 60.0), "C4 E4 +2");
}

/// Into a surround bed a strip pans with a room seen from above: dragging
/// it moves the track (one gesture), a double-click opens the panner, the
/// format menu offers the beds, and the meter widens for every channel.
#[test]
fn a_strip_feeding_a_bed_pans_in_the_room() {
    use faderframe_core::{ChannelLayout, SurroundFormat};
    let mut s = session();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S51),
    }))
    .unwrap();
    let pluck = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Pluck")
        .unwrap()
        .id;
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 760.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let i = MixerView::channel_tracks(&s)
        .iter()
        .position(|t| t.id == pluck)
        .unwrap();
    let t = s.project().track(pluck).unwrap();
    let l = view.layout_for(view.strip_rect(i, size), t, s.project());
    let plain = view.layout_for(
        view.strip_rect(i, size),
        t,
        &faderframe_project::Project::new("Plain", 48_000),
    );
    assert!(l.meter.w > plain.meter.w, "six meters need room");
    let c = l.pan_knob.center();
    let (actions, _) = run(&mut view, down(c, 1), size, &s);
    assert!(
        matches!(actions[..], [Action::BeginGesture(_)]),
        "{actions:?}"
    );
    let (actions, _) = run(
        &mut view,
        ViewEvent::PointerMove {
            pos: Point::new(c.x - l.pan_knob.w * 0.25, c.y + l.pan_knob.w * 0.5),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        size,
        &s,
    );
    match &actions[..] {
        [Action::Edit(Command::SetTrackSurround { track, pan })] => {
            assert_eq!(*track, pluck);
            assert!((pan.x + 0.5).abs() < 1e-4 && pan.y.abs() < 1e-4, "{pan:?}");
        }
        other => panic!("{other:?}"),
    }
    let (actions, _) = run(
        &mut view,
        ViewEvent::PointerUp {
            pos: c,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    assert!(matches!(actions[..], [Action::EndGesture]));
    let (actions, _) = run(&mut view, down(c, 2), size, &s);
    assert!(matches!(actions[..], [Action::ShowSurroundPanner(id)] if id == pluck));
    let formats = s.format_choices(pluck);
    assert_eq!(formats.len(), 2 + SurroundFormat::ALL.len());
    assert!(formats.iter().any(|f| f.label.starts_with("7.1.4")));
}

/// The master's record slot is its mono check; its menu chooses how it is
/// listened to (speakers or headphones) and, on a bed, its headphone
/// render mode for delivery.
#[test]
fn the_master_strip_has_a_mono_check_and_listening_choices() {
    use faderframe_core::{ChannelLayout, SurroundFormat};
    let mut s = session();
    let master = s.project().master_id().unwrap();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 760.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let l = view.layout_of(&s, master, size).unwrap();
    let (actions, _) = run(&mut view, down(l.record.center(), 1), size, &s);
    assert_eq!(actions, vec![Action::SetMonoCheck(true)]);
    s.dispatch(Action::SetMonoCheck(true)).unwrap();
    let (actions, _) = run(&mut view, down(l.record.center(), 1), size, &s);
    assert_eq!(actions, vec![Action::SetMonoCheck(false)]);
    let menu = |view: &mut MixerView, s: &Session| {
        let l = view.layout_of(s, master, size).unwrap();
        let ev = ViewEvent::PointerDown {
            pos: l.scribble.center(),
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        };
        let (_, req) = run(view, ev, size, s);
        match req
            .into_iter()
            .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
        {
            Some(HostRequest::ContextMenu { items, .. }) => items,
            _ => panic!("expected the master's menu"),
        }
    };
    let items = menu(&mut view, &s);
    let listen = items.iter().find(|i| i.label == "Listen").unwrap();
    let labels: Vec<&str> = listen.children.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels[0], "Speakers");
    assert!(labels.contains(&"Headphones · Mid (binaural)"));
    assert_eq!(listen.children[0].checked, Some(true));
    assert!(
        !items
            .iter()
            .any(|i| i.label.starts_with("Headphone Render"))
    );
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S714),
    }))
    .unwrap();
    let items = menu(&mut view, &s);
    assert!(
        items
            .iter()
            .any(|i| i.label.starts_with("Headphone Render"))
    );
}

/// With a console, the buses' input stage is its bus amplifier — Gain in
/// percent and Output in dB, a drag moving Gain up from 50 %, a
/// double-click back to unity — and the mixer takes the console's look until
/// the look is turned off.
#[test]
fn a_console_shows_its_bus_amplifiers_and_its_look() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    s.dispatch(Action::SetConsole { family: Some(1) }).unwrap();
    let bus = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.kind == TrackKind::Bus)
        .unwrap()
        .id;
    view.update_sends(&s);
    let mut canvas = faderframe_ui_canvas::RecordingPainter::default();
    view.paint(&mut canvas, size, &s, &Theme::default());
    assert_ne!(
        view.theme.console.panel_top,
        Theme::default().console.panel_top,
        "the console's look"
    );
    let area = view.layout_of(&s, bus, size).unwrap().preamp.unwrap();
    let knob = Point::new(area.x + area.w * 0.25, area.y + 30.0);
    assert_eq!(view.hit_test(knob, size, &s), Some(Hit::PreampKnob(bus, 2)));
    assert!(
        view.tooltip(knob, size, &s)
            .unwrap()
            .starts_with("Gain 50.0%")
    );
    let (mut actions, _) = run(&mut view, down(knob, 1), size, &s);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerMove {
            pos: Point::new(knob.x, knob.y - 20.0),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        size,
        &s,
    );
    actions.extend(a);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerUp {
            pos: knob,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    actions.extend(a);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    let amp = |s: &Session| s.project().track(bus).unwrap().preamp.clone().unwrap();
    let gain = preamp::value(&amp(&s), 2);
    assert!(gain > 0.5 && gain <= 1.0, "gain {gain}");
    assert_eq!(preamp::value(&amp(&s), 0), 0.0, "colour drive stays");
    let (actions, _) = run(&mut view, down(knob, 2), size, &s);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert_eq!(preamp::value(&amp(&s), 2), 0.5, "back to unity");
    let actions = press_drag_release(
        &mut view,
        &s,
        size,
        knob,
        Point::new(knob.x, knob.y + 200.0),
        Modifiers::NONE,
    );
    for action in actions {
        s.dispatch(action).unwrap();
    }
    assert_eq!(preamp::value(&amp(&s), 2), 0.0, "Gain reaches silence");
    s.dispatch(Action::Edit(Command::SetConsoleLook { look: false }))
        .unwrap();
    view.paint(&mut canvas, size, &s, &Theme::default());
    assert_eq!(
        view.theme.console.panel_top,
        Theme::default().console.panel_top
    );
}

/// The input stage's menu: on the master the console for the whole mix, on
/// a bus its own amplifier (following the console, another console's, in
/// the box), on a channel the microphone preamps.
#[test]
fn the_master_chooses_the_console_and_a_bus_its_amplifier() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1400.0, 900.0);
    s.dispatch(Action::SetConsole { family: Some(2) }).unwrap();
    view.update_sends(&s);
    let labels = |req: &[HostRequest<Action>]| -> Vec<String> {
        match req {
            [HostRequest::ContextMenu { items, .. }] => {
                items.iter().map(|i| i.label.clone()).collect()
            }
            _ => Vec::new(),
        }
    };
    let master = s.project().master_id().unwrap();
    let area = view.layout_of(&s, master, size).unwrap().preamp.unwrap();
    let (_, req) = run(
        &mut view,
        down(Point::new(area.center().x, area.y + 8.0), 1),
        size,
        &s,
    );
    let menu = labels(&req);
    assert!(menu.iter().any(|l| l == "British 73"), "{menu:?}");
    assert!(menu.iter().any(|l| l.starts_with("Off")), "{menu:?}");
    assert!(
        menu.iter().any(|l| l == "Drive"),
        "master colour drive remains available"
    );
    let bus = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.kind == TrackKind::Bus)
        .unwrap()
        .id;
    let area = view.layout_of(&s, bus, size).unwrap().preamp.unwrap();
    let (_, req) = run(
        &mut view,
        down(Point::new(area.center().x, area.y + 8.0), 1),
        size,
        &s,
    );
    let menu = labels(&req);
    assert!(
        menu.iter().any(|l| l.starts_with("Follow the Console")),
        "{menu:?}"
    );
    assert!(
        menu.iter().any(|l| l == "In the Box on This Bus"),
        "{menu:?}"
    );
    let audio = MixerView::channel_tracks(&s)[0].id;
    let area = view.layout_of(&s, audio, size).unwrap().preamp.unwrap();
    let (_, req) = run(&mut view, down(area.center(), 1), size, &s);
    assert!(
        labels(&req).iter().any(|l| l == "British 73"),
        "the preamps"
    );
}

/// Folders have strips of their own: narrow, the triangle folding what
/// they hold away (and back), mute reaching it, and a strip dragged onto a
/// folder's strip goes into the folder.
#[test]
fn folder_strips_fold_mute_and_take_dropped_strips() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1600.0, 900.0);
    view.update_sends(&s);
    view.update_strips(&s);
    let strips = |s: &Session| -> Vec<TrackId> {
        MixerView::channel_tracks(s).iter().map(|t| t.id).collect()
    };
    let i = MixerView::channel_tracks(&s)
        .iter()
        .position(|t| t.kind == TrackKind::Folder)
        .expect("the demo's folder");
    let folder = strips(&s)[i];
    let rect = view.strip_rect(i, size);
    assert_eq!(rect.w, FOLDER_W);
    let held = s.folder_contents(folder);
    assert!(held.iter().all(|t| strips(&s).contains(t)), "open: shown");
    // Fold it: what it holds goes from the mixer.
    let f = folder_layout(rect);
    let (a, _) = run(&mut view, down(f.fold.center(), 1), size, &s);
    assert_eq!(a, vec![Action::ToggleFolder(folder)]);
    s.dispatch(a[0].clone()).unwrap();
    assert!(
        held.iter().all(|t| !strips(&s).contains(t)),
        "closed: hidden"
    );
    assert!(strips(&s).contains(&folder));
    // Mute on the folder's strip.
    view.update_strips(&s);
    let i = strips(&s).iter().position(|t| *t == folder).unwrap();
    let f = folder_layout(view.strip_rect(i, size));
    let (a, _) = run(&mut view, down(f.mute.center(), 1), size, &s);
    for x in a {
        s.dispatch(x).unwrap();
    }
    assert!(s.project().track(folder).unwrap().mute);
    // The first strip dragged onto the folder's: into it.
    let first = strips(&s)[0];
    let from = view.layout_of(&s, first, size).unwrap().scribble.center();
    let onto = Point::new(view.strip_rect(i, size).center().x, from.y);
    let actions = press_drag_release(&mut view, &s, size, from, onto, Modifiers::NONE);
    assert!(
        actions.contains(&Action::MoveToFolder {
            tracks: vec![first],
            folder: Some(folder),
        }),
        "{actions:?}"
    );
    for x in actions {
        s.dispatch(x).unwrap();
    }
    assert_eq!(s.project().track(first).unwrap().folder, Some(folder));
}

/// A track with a compressor shows its gain reduction in a stripe left of
/// its meter (the stripe only where something compresses; a click there
/// does not move the fader).
#[test]
fn a_compressing_track_shows_its_gain_reduction() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1600.0, 900.0);
    view.update_sends(&s);
    view.update_strips(&s);
    // The demo's Bass has a compressor; a track without dynamics has none.
    let bass = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    assert!(s.gain_reduction(bass).is_some());
    let plain = s.add_track(TrackKind::Audio).unwrap();
    assert!(s.gain_reduction(plain).is_none());
    assert_eq!(s.gain_reduction_devices(bass), ["FaderFrame Compressor"]);
    // Bypassed, it is not counted.
    let comp = s
        .project()
        .track(bass)
        .unwrap()
        .inserts
        .iter()
        .find(|x| x.plugin.id == faderframe_core::builtin::COMPRESSOR)
        .unwrap()
        .id;
    let l = view.layout_of(&s, bass, size).unwrap();
    let stripe = reduction_rect(&l);
    assert_eq!(
        view.hit_test(stripe.center(), size, &s),
        Some(Hit::Reduction(bass))
    );
    let (a, _) = run(&mut view, down(stripe.center(), 1), size, &s);
    assert!(a.is_empty(), "{a:?}");
    s.dispatch(Action::Edit(Command::SetPluginBypass {
        track: bass,
        plugin: comp,
        bypass: true,
    }))
    .unwrap();
    assert!(s.gain_reduction(bass).is_none());
}

/// A meter's right-click menu sets what it shows (this strip or every
/// one); each mode paints, and the tooltip reads it in its terms.
#[test]
fn a_meters_menu_chooses_what_it_shows() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1600.0, 900.0);
    view.update_sends(&s);
    view.update_strips(&s);
    let bass = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let l = view.layout_of(&s, bass, size).unwrap();
    let (_, requests) = run(&mut view, right(l.meter.center()), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    for want in [
        "Peak",
        "Peak + RMS",
        "VU",
        "PPM (EBU)",
        "K-20",
        "K-14",
        "K-12",
    ] {
        assert!(labels.contains(&want), "{labels:?}");
    }
    assert_eq!(items[0].checked, Some(true), "Peak now");
    let vu = items.iter().find(|i| i.label == "VU").unwrap();
    s.dispatch(vu.action.clone().unwrap()).unwrap();
    assert_eq!(s.meter_mode(bass), MeterMode::Vu);
    let other = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    assert_eq!(s.meter_mode(other), MeterMode::Peak, "only this strip");
    assert!(
        view.tooltip(l.meter.center(), size, &s)
            .unwrap()
            .starts_with("VU meter")
    );
    // Every strip: K-14.
    let every = items.iter().find(|i| i.label == "Every Strip").unwrap();
    let k14 = every.children.iter().find(|i| i.label == "K-14").unwrap();
    s.dispatch(k14.action.clone().unwrap()).unwrap();
    assert_eq!(s.meter_mode(bass), MeterMode::K14);
    assert_eq!(s.meter_mode(other), MeterMode::K14);
    for mode in MeterMode::ALL {
        s.dispatch(Action::SetMeterMode {
            track: None,
            mode: Some(mode),
        })
        .unwrap();
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(
            view.tooltip(l.meter.center(), size, &s).is_some(),
            "{mode:?}"
        );
    }
}

/// The VU meter bridge: a VU meter over the strips; the strips below it
/// answer where they are drawn.
#[test]
fn the_meter_bridge_puts_vu_meters_over_the_strips() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1600.0, 900.0);
    s.dispatch(Action::SetMeterBridge(true)).unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    let vus = p.texts().iter().filter(|t| **t == "VU").count();
    assert!(
        vus >= MixerView::channel_tracks(&s)
            .iter()
            .filter(|t| t.kind.has_audio())
            .count()
    );
    // The bridge's tooltip names the strip under it.
    let body = Size::new(size.w, size.h - BRIDGE_H);
    let bass = MixerView::channel_tracks(&s)
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let l = view.layout_of(&s, bass, body).unwrap();
    let over = Point::new(l.meter.center().x, BRIDGE_H / 2.0);
    assert!(view.tooltip(over, size, &s).unwrap().starts_with("Bass"));
    // The strip's meter, drawn lower by the bridge, answers there.
    let meter = Point::new(l.meter.center().x, l.meter.center().y + BRIDGE_H);
    assert!(
        view.tooltip(meter, size, &s)
            .unwrap()
            .contains("Peak meter")
    );
    let (_, requests) = run(&mut view, right(over), size, &s);
    assert!(matches!(
        requests.first(),
        Some(HostRequest::ContextMenu { .. })
    ));
}

#[test]
fn a_dual_panned_strip_has_a_knob_per_channel() {
    use faderframe_core::{DualPan, PanMode};
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 760.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let t = MixerView::channel_tracks(&s)
        .into_iter()
        .find(|t| s.project().pans_stereo(t) && t.kind != TrackKind::Master)
        .expect("a stereo strip")
        .id;
    // Balance first: its knob's menu offers the modes.
    let l = view.layout_of(&s, t, size).unwrap();
    assert_eq!(
        view.hit_test(l.pan_knob.center(), size, &s),
        Some(Hit::Pan(t))
    );
    let labels = menu_labels(run(&mut view, right(l.pan_knob.center()), size, &s).1);
    assert!(labels.iter().any(|l| l == "Balance") && labels.iter().any(|l| l == "Dual Pan"));
    s.dispatch(Action::Edit(Command::SetTrackPanMode {
        track: t,
        mode: PanMode::Dual,
    }))
    .unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let texts = p.texts();
    assert!(
        texts.contains(&"L100") && texts.contains(&"R100"),
        "{texts:?}"
    );

    let l = view.layout_of(&s, t, size).unwrap();
    let [(left, left_value), (right_knob, _)] = dual_pan_rects(&l);
    assert!(!left.intersects(&right_knob));
    assert_eq!(
        view.hit_test(left.center(), size, &s),
        Some(Hit::DualPan(t, false))
    );
    assert_eq!(
        view.hit_test(right_knob.center(), size, &s),
        Some(Hit::DualPan(t, true))
    );
    // Between the two knobs nothing reaches the (hidden) balance.
    let between = Point::new((left.right() + right_knob.x) * 0.5, left.center().y);
    assert!(!matches!(
        view.hit_test(between, size, &s),
        Some(Hit::Pan(_))
    ));

    // Dragging the right knob up moves only the right channel, as one gesture.
    let from = right_knob.center();
    let mut actions = run(&mut view, down(from, 1), size, &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(from.x, from.y + 45.0),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &s,
        )
        .0,
    );
    assert_eq!(actions.first(), Some(&Action::BeginGesture("Pan".into())));
    match actions.last() {
        Some(Action::Edit(Command::SetTrackDualPan { track, pan })) => {
            assert_eq!(*track, t);
            assert_eq!(pan.left, -1.0);
            assert!(pan.right < 0.6 && pan.right > 0.4, "{}", pan.right);
        }
        other => panic!("{other:?}"),
    }
    run(
        &mut view,
        ViewEvent::PointerUp {
            pos: from,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );

    // A double-click sends a channel back to its side.
    s.dispatch(Action::Edit(Command::SetTrackDualPan {
        track: t,
        pan: DualPan {
            left: 0.2,
            right: 0.2,
        },
    }))
    .unwrap();
    let (actions, _) = run(&mut view, down(left.center(), 2), size, &s);
    assert_eq!(
        actions,
        vec![Action::Edit(Command::SetTrackDualPan {
            track: t,
            pan: DualPan {
                left: -1.0,
                right: 0.2
            }
        })]
    );
    // Its value takes a typed position.
    let (_, requests) = run(&mut view, down(left_value.center(), 1), size, &s);
    let Some(HostRequest::TextInput {
        initial, commit, ..
    }) = requests.into_iter().next()
    else {
        panic!("a text field")
    };
    assert_eq!(initial, "R20");
    assert_eq!(
        commit("L40"),
        Some(Action::Edit(Command::SetTrackDualPan {
            track: t,
            pan: DualPan {
                left: -0.4,
                right: 0.2
            }
        }))
    );
    // Right-click: that side's MIDI learn and the way back to balance.
    let labels = menu_labels(run(&mut view, right(left.center()), size, &s).1);
    assert!(labels[0].ends_with("Left Pan"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Balance"));
}

#[test]
fn a_strip_into_a_bed_turns_by_vbap_or_balances_on_its_knob() {
    use faderframe_core::surround::{StereoSource, SurroundPanner};
    use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
    let mut s = session();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S51),
    }))
    .unwrap();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 760.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let t = MixerView::channel_tracks(&s)
        .into_iter()
        .find(|t| t.layout == ChannelLayout::Stereo && s.project().surround_panned(t).is_some())
        .expect("a stereo track into the bed")
        .id;
    // VBAP: dragging across the mini room turns the track round.
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            panner: SurroundPanner::Vbap,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let l = view.layout_of(&s, t, size).unwrap();
    let from = l.pan_knob.center();
    let mut actions = run(&mut view, down(from, 1), size, &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(from.x - l.pan_knob.w * 0.25, from.y),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &s,
        )
        .0,
    );
    match actions.last() {
        // A quarter of the room's width to the left: 90° to the left.
        Some(Action::Edit(Command::SetTrackSurround { pan, .. })) => {
            assert!((pan.azimuth - 90.0).abs() < 1.0, "{}", pan.azimuth);
            assert_eq!((pan.x, pan.y), (0.0, 1.0), "the room's place kept");
        }
        other => panic!("{other:?}"),
    }
    run(
        &mut view,
        ViewEvent::PointerUp {
            pos: from,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    // Balanced on L and R: the strip shows its balance knob again.
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            stereo: StereoSource::Balance,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let l = view.layout_of(&s, t, size).unwrap();
    assert_eq!(
        view.tooltip_body(l.pan_knob.center(), size, &s)
            .map(|t| t.starts_with("Balance")),
        Some(true)
    );
    let from = l.pan_knob.center();
    let mut actions = run(&mut view, down(from, 1), size, &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(from.x, from.y - 40.0),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &s,
        )
        .0,
    );
    assert!(
        matches!(
            actions.last(),
            Some(Action::Edit(Command::SetTrackPan { track, pan })) if *track == t && *pan > 0.0
        ),
        "{actions:?}"
    );
}

/// Shift-click on a strip's name selects the strips from the one last
/// clicked to it, as the mixer shows them.
#[test]
fn shift_click_selects_the_strips_between() {
    let mut s = session();
    let mut view = MixerView::new(Theme::default());
    let size = Size::new(1600.0, 700.0);
    view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
    let strips: Vec<TrackId> = MixerView::channel_tracks(&s).iter().map(|t| t.id).collect();
    let name = |view: &MixerView, s: &Session, i: usize| {
        view.scribble_rect(s, strips[i], size).unwrap().center()
    };
    let click = |view: &mut MixerView, s: &mut Session, at: Point, mods: Modifiers| {
        let down = ViewEvent::PointerDown {
            pos: at,
            button: PointerButton::Primary,
            modifiers: mods,
            clicks: 1,
        };
        let up = ViewEvent::PointerUp {
            pos: at,
            button: PointerButton::Primary,
            modifiers: mods,
        };
        for ev in [down, up] {
            for a in run(view, ev, size, s).0 {
                s.dispatch(a).unwrap();
            }
        }
    };
    let first = name(&view, &s, 0);
    click(&mut view, &mut s, first, Modifiers::NONE);
    let third = name(&view, &s, 3);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    click(&mut view, &mut s, third, shift);
    let want: std::collections::BTreeSet<_> = strips[..4].iter().copied().collect();
    assert_eq!(s.selection.tracks, want);
}

/// Cue mode: with a cue mix shown, a strip's fader, pan and mute set its
/// send there (one gesture per drag), solo does nothing, and the master
/// section is the cue mix's own (level, mute, click); the strips sit under
/// the cue bar.
#[test]
fn in_a_cue_mix_the_strips_set_its_sends() {
    use faderframe_session::cues::CueOp;
    let mut s = session();
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    let cue = s.mixer_cue().unwrap().id;
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 800.0);
    let bar = cue::CUE_BAR_H;
    let below = Size::new(size.w, size.h - bar);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let texts = p.texts();
    assert!(
        texts.contains(&"CUE 1"),
        "the bar and the cue master: {texts:?}"
    );
    assert!(texts.contains(&"MAIN MIX"));
    assert!(
        !texts.contains(&"MASTER"),
        "the master section is the cue mix's"
    );
    let at = |r: Rect| Point::new(r.center().x, r.center().y + bar);
    let bass = MixerView::channel_tracks(&s)[1].id;
    let l = view.layout_of(&s, bass, below).unwrap();

    let (a, _) = run(&mut view, down(at(l.mute), 1), size, &s);
    assert_eq!(
        a,
        vec![Action::Edit(Command::SetCueSendOn {
            track: bass,
            cue,
            on: false
        })]
    );
    let (a, _) = run(&mut view, down(at(l.solo), 1), size, &s);
    assert!(a.is_empty(), "solo is the main mix's: {a:?}");
    let (a, _) = run(&mut view, down(at(l.pan_knob), 2), size, &s);
    assert_eq!(
        a,
        vec![Action::Edit(Command::SetCueSendPan {
            track: bass,
            cue,
            pan: 0.0
        })]
    );
    // A fader drag: the send level, one gesture.
    let geo = FaderGeometry::new(l.fader, &theme);
    let level = s.cue_send(s.project().track(bass).unwrap(), cue).level_db;
    let cap = geo.cap_rect(view.law.db_to_position(level)).center();
    let cap = Point::new(cap.x, cap.y + bar);
    let (mut all, _) = run(&mut view, down(cap, 1), size, &s);
    assert_eq!(all, vec![Action::BeginGesture("Cue Level".into())]);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerMove {
            pos: Point::new(cap.x, cap.y - 30.0),
            modifiers: Modifiers::NONE,
            dragging: true,
        },
        size,
        &s,
    );
    all.extend(a);
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
    assert!(all.iter().any(|a| matches!(
        a,
        Action::Edit(Command::SetCueSendLevel { track, db, .. }) if *track == bass && *db > level
    )));
    assert!(
        !all.iter()
            .any(|a| matches!(a, Action::Edit(Command::SetTrackVolume { .. }))),
        "the main fader never moves"
    );
    // The master section: the cue mix's mute and click.
    let master = s.project().master_id().unwrap();
    let m = view.layout_of(&s, master, below).unwrap();
    let (a, _) = run(&mut view, down(at(m.mute), 1), size, &s);
    assert_eq!(a, vec![Action::Edit(Command::SetCueMute { cue, on: true })]);
    let (a, _) = run(&mut view, down(at(m.record), 1), size, &s);
    assert_eq!(
        a,
        vec![Action::Edit(Command::SetCueClick {
            cue,
            on: false,
            db: 0.0
        })]
    );
    // Applied: the bass's send is off, its main mute is not.
    s.dispatch(Action::Edit(Command::SetCueSendOn {
        track: bass,
        cue,
        on: false,
    }))
    .unwrap();
    assert!(MixerView::mute_shown(&s, s.project().track(bass).unwrap()));
    assert!(!s.project().track(bass).unwrap().mute);
}

/// The cue bar: Main Mix and each cue mix to show, "+" for a new one,
/// Copy Main Mix and Pre/Post for the one shown, its menu on a right-click.
#[test]
fn the_cue_bar_chooses_what_the_faders_set() {
    use faderframe_session::cues::CueOp;
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 800.0);
    // No cue mixes: no bar.
    assert_eq!(view.cue_bar_h(&s), 0.0);
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    let cue = s.mixer_cue().unwrap().id;
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    assert_eq!(view.cue_bar_h(&s), cue::CUE_BAR_H);
    let items = view.bar_items(size.w, &s);
    let find = |want: BarHit| {
        items
            .iter()
            .find(|(_, h, _)| *h == want)
            .map(|(r, _, _)| r.center())
            .unwrap()
    };
    let (a, _) = run(&mut view, down(find(BarHit::Main), 1), size, &s);
    assert_eq!(a, vec![Action::Cue(CueOp::Show(None))]);
    let (a, _) = run(&mut view, down(find(BarHit::Cue(cue)), 1), size, &s);
    assert_eq!(a, vec![Action::Cue(CueOp::Show(Some(cue)))]);
    let (a, _) = run(&mut view, down(find(BarHit::New), 1), size, &s);
    assert_eq!(a, vec![Action::Cue(CueOp::New)]);
    let (a, _) = run(&mut view, down(find(BarHit::CopyMain), 1), size, &s);
    assert_eq!(a, vec![Action::Cue(CueOp::CopyMainMix(cue))]);
    let (a, _) = run(&mut view, down(find(BarHit::Tap), 1), size, &s);
    assert_eq!(
        a,
        vec![Action::Cue(CueOp::SetTap {
            cue,
            tap: SendTap::PostFader
        })]
    );
    let (_, r) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: find(BarHit::Cue(cue)),
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        size,
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = r.first() else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"Remove Cue Mix"), "{labels:?}");
    assert!(labels.contains(&"Copy Main Mix"));
    assert!(labels.iter().any(|l| l.starts_with("Outputs (Out 3-4)")));
    // Back on the main mix the faders are the main mix's again.
    s.dispatch(Action::Cue(CueOp::Show(None))).unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.texts().contains(&"MASTER"));
    let bar = cue::CUE_BAR_H;
    let first = MixerView::channel_tracks(&s)[0];
    let l = view
        .layout_of(&s, first.id, Size::new(size.w, size.h - bar))
        .unwrap();
    let (a, _) = run(
        &mut view,
        down(Point::new(l.mute.center().x, l.mute.center().y + bar), 1),
        size,
        &s,
    );
    assert_eq!(
        a,
        vec![Action::Edit(Command::SetTrackMute {
            track: first.id,
            on: true
        })]
    );
}

/// The cue bar's TALK button: talking while pressed (let go anywhere),
/// latched by Shift-click; its menu learns MIDI and chooses which cue
/// mixes hear it. In cue mode a fader's menu learns the cue send (the
/// master's: the cue mix's level).
#[test]
fn the_talk_button_talks_while_held_and_latches_with_shift() {
    use faderframe_automation::{AutomationTarget, CueSendParam};
    use faderframe_project::MappingTarget;
    use faderframe_session::cues::{CueOp, TalkOp};
    let mut s = session();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(1400.0, 800.0);
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    let cue = s.mixer_cue().unwrap().id;
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let talk = view
        .bar_items(size.w, &s)
        .iter()
        .find(|(_, h, _)| *h == BarHit::Talk)
        .map(|(r, _, _)| r.center())
        .unwrap();
    let (a, _) = run(&mut view, down(talk, 1), size, &s);
    assert_eq!(a, vec![Action::Talkback(TalkOp::Press)]);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerUp {
            pos: Point::new(300.0, 500.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        },
        size,
        &s,
    );
    assert_eq!(a, vec![Action::Talkback(TalkOp::Release)]);
    let (a, _) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: talk,
            button: PointerButton::Primary,
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            clicks: 1,
        },
        size,
        &s,
    );
    assert_eq!(a, vec![Action::Talkback(TalkOp::Latch(true))]);
    let (a, _) = run(&mut view, down(talk, 2), size, &s);
    assert_eq!(a, vec![Action::Talkback(TalkOp::Latch(true))]);
    s.dispatch(Action::Talkback(TalkOp::Latch(true))).unwrap();
    let (a, _) = run(&mut view, down(talk, 1), size, &s);
    assert_eq!(
        a,
        vec![Action::Talkback(TalkOp::Latch(false))],
        "a click unlatches"
    );
    let (_, r) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: talk,
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        size,
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = r.first() else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels[0], "MIDI Learn…");
    assert!(labels.contains(&"Cue 1 hears talkback"), "{labels:?}");
    assert_eq!(
        items[0].action,
        Some(Action::MidiLearn(MappingTarget::Talkback))
    );
    // In cue mode a fader's menu learns the send to the cue mix.
    let bar = cue::CUE_BAR_H;
    let below = Size::new(size.w, size.h - bar);
    let bass = MixerView::channel_tracks(&s)[1].id;
    let l = view.layout_of(&s, bass, below).unwrap();
    let (_, r) = run(
        &mut view,
        ViewEvent::PointerDown {
            pos: Point::new(l.fader.center().x, l.fader.center().y + bar),
            button: PointerButton::Secondary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        size,
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = r.first() else {
        panic!("a menu");
    };
    let learn = items
        .iter()
        .find(|i| i.label.starts_with("MIDI Learn Level"))
        .expect("learns the level");
    assert_eq!(
        learn.children[0].action,
        Some(Action::MidiLearn(MappingTarget::Parameter {
            track: bass,
            target: AutomationTarget::CueSend {
                cue,
                what: CueSendParam::Level
            }
        }))
    );
}

/// The Inspector's channel: one track's strip filling the view — any
/// kind, the master too — with the console strip's controls, without its
/// inserts (the Inspector lists the devices) or wooden cheeks.
#[test]
fn a_single_strip_shows_one_track_without_its_inserts() {
    let s = session();
    let theme = Theme::default();
    let mut view = MixerView::single_strip(theme.clone());
    let size = Size::new(260.0, 620.0);
    let texts = |view: &mut MixerView| {
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &theme);
        p.texts().iter().map(|t| t.to_string()).collect::<Vec<_>>()
    };
    // Nothing shown yet: an empty view, nothing to press.
    assert!(texts(&mut view).is_empty());
    assert_eq!(view.hit_test(Point::new(130.0, 300.0), size, &s), None);
    let by_name = |n: &str| s.project().tracks.iter().find(|t| t.name == n).unwrap();
    let bass = by_name("Bass");
    view.show_track(Some(bass.id));
    assert_eq!(view.shown_track(), Some(bass.id));
    let t = texts(&mut view);
    assert!(t.contains(&"Bass".to_string()), "{t:?}");
    assert!(!t.iter().any(|x| x.starts_with("INSERTS")), "{t:?}");
    assert!(!t.contains(&"MASTER".to_string()), "{t:?}");
    // The whole width is the strip; its mute mutes the track shown.
    let l = view.layout_for(view.master_rect(size), bass, s.project());
    assert!(l.inserts.is_none());
    assert!((l.strip.w - size.w).abs() < 0.5, "no cheeks: {}", l.strip.w);
    assert_eq!(
        view.hit_test(l.mute.center(), size, &s),
        Some(Hit::Mute(bass.id))
    );
    let (actions, _) = run(&mut view, down(l.mute.center(), 1), size, &s);
    assert_eq!(
        actions,
        vec![Action::Edit(Command::SetTrackMute {
            track: bass.id,
            on: true
        })]
    );
    assert_eq!(view.accessible_name().as_deref(), Some("Channel"));
    let nodes = view.accessible(size, &s);
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].label.starts_with("Bass"), "{}", nodes[0].label);
    // A folder: its folder strip.
    let folder = by_name("Arpeggios");
    view.show_track(Some(folder.id));
    texts(&mut view);
    let f = folder_layout(view.master_rect(size));
    assert_eq!(
        view.hit_test(f.fold.center(), size, &s),
        Some(Hit::Fold(folder.id))
    );
    // The master: its strip with the mono check.
    let master = s.project().master().unwrap();
    view.show_track(Some(master.id));
    let t = texts(&mut view);
    assert!(t.contains(&"MASTER".to_string()), "{t:?}");
    let l = view.layout_for(view.master_rect(size), master, s.project());
    assert_eq!(
        view.hit_test(l.record.center(), size, &s),
        Some(Hit::MonoCheck)
    );
}

/// A folder's strip has a Group switch under its solo (a folder bus's
/// full strip: where a track's record button is); pressing it groups or
/// ungroups what the folder holds; it is lit, named in its tooltip and a
/// toggle for screen readers.
#[test]
fn a_folder_strip_has_a_group_switch() {
    let mut s = session();
    let folder = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Arpeggios")
        .unwrap()
        .id;
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(4000.0, 900.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.texts().contains(&"G"), "{:?}", p.texts());
    let i = MixerView::channel_tracks(&s)
        .iter()
        .position(|t| t.id == folder)
        .unwrap();
    let f = folder_layout(view.strip_rect(i, size));
    assert!(!f.group.intersects(&f.solo) && !f.group.intersects(&f.count));
    assert_eq!(
        view.hit_test(f.group.center(), size, &s),
        Some(Hit::FolderGroup(folder))
    );
    let (actions, _) = run(&mut view, down(f.group.center(), 1), size, &s);
    assert_eq!(actions, vec![Action::SetFolderGroup { folder, on: true }]);
    s.dispatch(actions[0].clone()).unwrap();
    assert!(s.folder_grouped(folder));
    let tip = view.tooltip(f.group.center(), size, &s).unwrap();
    assert!(tip.starts_with("Group Arpeggios: on"), "{tip}");
    let nodes = view.accessible(size, &s);
    let node = nodes
        .iter()
        .find(|n| n.label == "Arpeggios folder")
        .expect("the folder's strip");
    assert_eq!(node.children.len(), 1);
    assert_eq!(
        (node.children[0].label.as_str(), node.children[0].checked),
        ("Group", Some(true))
    );
    // A folder bus: the switch where a track arms.
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    view.paint(&mut RecordingPainter::new(), size, &s, &theme);
    let l = view.layout_of(&s, folder, size).unwrap();
    assert_eq!(
        view.hit_test(l.record.center(), size, &s),
        Some(Hit::FolderGroup(folder))
    );
    let (actions, _) = run(&mut view, down(l.record.center(), 1), size, &s);
    assert_eq!(actions, vec![Action::SetFolderGroup { folder, on: false }]);
    s.dispatch(actions[0].clone()).unwrap();
    assert!(!s.folder_grouped(folder));
    let nodes = view.accessible(size, &s);
    let node = nodes
        .iter()
        .find(|n| n.label == "Arpeggios folder bus")
        .expect("the folder bus's strip");
    assert!(
        node.children
            .iter()
            .any(|c| c.label == "Group" && c.checked == Some(false))
    );
}

/// A folder bus is a full strip (a bus's fader, pan, inserts, sends and
/// meter) named as one, with a triangle in its header that folds it; the
/// strips of what it holds name it as their output, and their output menu
/// offers it as the default and the master by name past it. Its pan knob
/// sets its pan, as every bus's and return's does.
#[test]
fn a_folder_bus_is_a_full_strip_that_folds() {
    let mut s = session();
    let (bass, pad) = (
        s.project()
            .tracks
            .iter()
            .find(|t| t.name == "Bass")
            .unwrap()
            .id,
        s.project()
            .tracks
            .iter()
            .find(|t| t.name == "Pad")
            .unwrap()
            .id,
    );
    s.dispatch(Action::NewFolder {
        tracks: vec![bass, pad],
    })
    .unwrap();
    // The new folder (selected).
    let folder = *s.selection.tracks.iter().next().unwrap();
    let name = s.project().track(folder).unwrap().name.clone();
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    let theme = Theme::default();
    let mut view = MixerView::new(theme.clone());
    let size = Size::new(2400.0, 900.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.texts().contains(&"FOLDER BUS"), "{:?}", p.texts());
    let i = MixerView::channel_tracks(&s)
        .iter()
        .position(|t| t.id == folder)
        .unwrap();
    assert_eq!(
        view.strip_rect(i, size).w,
        theme.console.strip_width,
        "full width"
    );
    let l = view.layout_of(&s, folder, size).unwrap();
    assert!(
        l.inserts.is_some() && l.sends.is_some(),
        "devices and sends"
    );
    assert_eq!(
        view.hit_test(l.pan_knob.center(), size, &s),
        Some(Hit::Pan(folder))
    );
    assert_eq!(
        view.hit_test(l.fader.center(), size, &s)
            .map(|h| matches!(h, Hit::FaderTrack(_) | Hit::FaderCap(_))),
        Some(true)
    );
    // Its pan knob dragged: its own pan, one gesture.
    let from = l.pan_knob.center();
    let mut actions = run(&mut view, down(from, 1), size, &s).0;
    actions.extend(
        run(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(from.x, from.y + 45.0),
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
                pos: from,
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            size,
            &s,
        )
        .0,
    );
    for a in actions {
        s.dispatch(a).unwrap();
    }
    assert!(s.project().track(folder).unwrap().pan < -0.3);
    // What it holds names it as its output; the menu: it first (the
    // default), the master by name.
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let label = format!("→ {name}");
    assert!(p.texts().contains(&label.as_str()), "{:?}", p.texts());
    let master = s.project().master_id().unwrap();
    let to_default = s.project().track(bass).unwrap().output == OutputRouting::Master;
    let l = view.layout_of(&s, bass, size).unwrap();
    let (_, req) = run(&mut view, down(l.output.center(), 1), size, &s);
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("the output menu");
    };
    assert_eq!(items[0].label, format!("{name} (Folder Bus)"));
    assert_eq!(items[0].checked, Some(to_default));
    let named = items.iter().find(|i| i.label == "Master").unwrap();
    assert_eq!(
        named.action,
        Some(Action::Edit(Command::SetTrackOutput {
            track: bass,
            output: OutputRouting::Track { track: master },
        }))
    );
    assert!(
        !items
            .iter()
            .any(|i| i.label == format!("{name} (Folder Bus)") && i.label != items[0].label)
    );
    // Its triangle folds it: what it holds leaves the mixer.
    let l = view.layout_of(&s, folder, size).unwrap();
    let fold = folder_bus_fold(&l).center();
    assert_eq!(view.hit_test(fold, size, &s), Some(Hit::Fold(folder)));
    let (a, _) = run(&mut view, down(fold, 1), size, &s);
    assert_eq!(a, vec![Action::ToggleFolder(folder)]);
    s.dispatch(a[0].clone()).unwrap();
    let shown: Vec<TrackId> = MixerView::channel_tracks(&s).iter().map(|t| t.id).collect();
    assert!(shown.contains(&folder) && !shown.contains(&bass) && !shown.contains(&pad));
}

/// The strip menu's Time Base: musical or time-locked.
#[test]
fn the_strip_menu_chooses_the_time_base() {
    let mut s = session();
    let t = MixerView::channel_tracks(&s)[0].clone();
    let menu = |s: &Session, t: &Track| {
        let HostRequest::ContextMenu { items, .. } =
            MixerView::track_menu(s, t, Point::new(0.0, 0.0))
        else {
            panic!("a menu")
        };
        items
            .into_iter()
            .find(|i| i.label == "Time Base")
            .unwrap()
            .children
    };
    let items = menu(&s, &t);
    let labels: Vec<_> = items
        .iter()
        .map(|i| (i.label.as_str(), i.checked))
        .collect();
    assert_eq!(
        labels,
        vec![("Musical", Some(true)), ("Time-locked", Some(false))]
    );
    s.dispatch(items[1].action.clone().unwrap()).unwrap();
    assert!(s.is_time_locked(t.id));
    let t = s.project().track(t.id).unwrap().clone();
    assert_eq!(menu(&s, &t)[1].checked, Some(true));
}
