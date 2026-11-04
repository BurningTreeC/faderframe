use super::*;
use faderframe_engine::EngineConfig;
use faderframe_ui_canvas::RecordingPainter;

#[test]
fn paints_every_pane_and_opens_its_menus() {
    let s = Session::demo(EngineConfig::default()).unwrap();
    let theme = Theme::default();
    let mut view = ToolsView::new(theme.clone());
    let size = Size::new(1300.0, 380.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let l = view.layout(size);
    for r in [l.loudness, l.level, l.phase, l.spectrum] {
        assert!(r.w > 60.0 && r.h > 200.0, "{r:?}");
    }
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    let down = |pos: Point| ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: faderframe_ui_canvas::Modifiers::NONE,
        clicks: 1,
    };
    view.event(&down(l.source.center()), size, &s, &mut cx);
    view.event(&down(l.target.center()), size, &s, &mut cx);
    view.event(&down(l.reset.center()), size, &s, &mut cx);
    assert_eq!(requests.len(), 2);
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!()
    };
    assert!(items.len() > 3, "master and the tracks with audio");
    assert!(actions.contains(&Action::ResetAnalysis));
    assert_eq!(format_lufs(-14.24), "−14.2");
    assert_eq!(format_lufs(f64::NEG_INFINITY), "−∞");
}

#[test]
fn the_codec_pane_shows_while_the_monitor_is_on() {
    use faderframe_session::codec::{CodecHear, CodecOp};
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let theme = Theme::default();
    let mut view = ToolsView::new(theme.clone());
    let size = Size::new(1400.0, 380.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(view.layout(size).codec.is_none(), "off: no pane");
    assert!(!p.texts().contains(&"CODEC MONITOR"));

    s.dispatch(Action::Codec(CodecOp::Platform("apple-music".into())))
        .unwrap();
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    let l = view.layout(size);
    let c = l.codec.as_ref().unwrap();
    assert!(c.pane.w >= 230.0 && l.spectrum.w > 60.0);
    let texts = p.texts();
    assert!(texts.contains(&"CODEC MONITOR"));
    assert!(texts.contains(&"Apple Music ▾"));
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Apple Music · AAC 256 kb/s"))
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("until the song is measured"))
    );

    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    let down = |pos: Point| ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: faderframe_ui_canvas::Modifiers::NONE,
        clicks: 1,
    };
    let difference = c.hear[2].1.center();
    let (choose, reset) = (c.choose.center(), c.reset.center());
    view.event(&down(difference), size, &s, &mut cx);
    view.event(&down(choose), size, &s, &mut cx);
    view.event(&down(reset), size, &s, &mut cx);
    assert_eq!(
        actions,
        [
            Action::Codec(CodecOp::Hear(CodecHear::Difference)),
            Action::Codec(CodecOp::ResetMeters)
        ]
    );
    let HostRequest::ContextMenu { items, .. } = &requests[0] else {
        panic!()
    };
    let checked: Vec<&str> = items
        .iter()
        .filter(|i| i.checked == Some(true))
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(checked.len(), 2, "{checked:?}");
    assert!(checked[0].starts_with("Apple Music"), "{checked:?}");
    assert_eq!(
        view.tooltip(difference, size, &s).unwrap(),
        "Hear what the codec removed or added, alone"
    );
}

mod keyboard {
    use super::*;
    use crate::access::{Limits, Readings};
    use faderframe_ui_canvas::access::{focus_order, press_key};
    use faderframe_ui_canvas::{AccessNode, AccessRole, Key, Modifiers};

    const SIZE: Size = Size::new(1400.0, 380.0);

    fn named(view: &ToolsView, s: &Session, label: &str) -> AccessNode<Action> {
        let nodes = view.accessible(SIZE, s);
        focus_order(&nodes)
            .into_iter()
            .find(|n| n.label.starts_with(label))
            .cloned()
            .unwrap_or_else(|| panic!("no control '{label}'"))
    }

    fn key(view: &mut ToolsView, s: &mut Session, label: &str, k: Key) -> Vec<HostRequest<Action>> {
        let id = named(view, s, label).id;
        let nodes = view.accessible(SIZE, s);
        let (actions, requests) = press_key(view, &nodes, id, k, Modifiers::NONE, SIZE, s)
            .unwrap_or_else(|| panic!("{k:?} does something on '{label}'"));
        for a in actions {
            s.dispatch(a).unwrap();
        }
        requests
    }

    /// The toolbar's menus and toggles, then every reading as a meter the
    /// keyboard visits; the codec pane's controls while the monitor is on.
    #[test]
    fn the_readings_are_meters_the_keyboard_visits() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let mut view = ToolsView::new(Theme::default());
        assert_eq!(view.accessible_name().as_deref(), Some("Tools"));
        let nodes = view.accessible(SIZE, &s);
        let names: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(
            names,
            [
                "Tools toolbar",
                "Loudness, EBU R128",
                "Level, dBFS",
                "Phase",
                "Spectrum of the mid signal"
            ]
        );
        // Menus open with Enter.
        for b in ["Source: Master", "Loudness target: ", "Level scale: dBFS"] {
            let req = key(&mut view, &mut s, b, Key::Enter);
            assert!(
                matches!(req.first(), Some(HostRequest::ContextMenu { items, .. }) if !items.is_empty()),
                "{b}"
            );
        }
        // Toggles: Reset on Play (the session's), Announce Limits (the
        // view's, said).
        let on = s.analysis_settings().reset_on_play;
        assert_eq!(named(&view, &s, "Reset on play").checked, Some(on));
        key(&mut view, &mut s, "Reset on play", Key::Enter);
        assert_eq!(s.analysis_settings().reset_on_play, !on);
        let announce = named(&view, &s, "Announce limits");
        assert_eq!(
            (announce.role, announce.checked),
            (AccessRole::ToggleButton, Some(false))
        );
        let req = key(&mut view, &mut s, "Announce limits", Key::Enter);
        assert!(matches!(
            req.first(),
            Some(HostRequest::Announce(t)) if t == "Announcing limits"
        ));
        assert_eq!(named(&view, &s, "Announce limits").checked, Some(true));
        // The readings: meters with values, nothing measured yet.
        let integrated = named(&view, &s, "Integrated loudness");
        assert_eq!(integrated.role, AccessRole::Meter);
        assert_eq!(integrated.spoken(), "Integrated loudness, no reading");
        for m in [
            "True peak",
            "Loudness range",
            "Left peak",
            "Right RMS",
            "Correlation",
        ] {
            let n = named(&view, &s, m);
            assert_eq!(n.role, AccessRole::Meter, "{m}");
            let v = n.value.as_ref().unwrap();
            assert!(v.now >= v.min && v.now <= v.max, "{m}: {v:?}");
        }
        assert_eq!(named(&view, &s, "Silent").role, AccessRole::Label);
        // The codec pane.
        s.dispatch(Action::Codec(CodecOp::Platform("apple-music".into())))
            .unwrap();
        let hear = named(&view, &s, "Hear the difference");
        assert_eq!(hear.role, AccessRole::ToggleButton);
        key(&mut view, &mut s, "Hear the difference", Key::Enter);
        assert_eq!(s.codec_status().hear, CodecHear::Difference);
        assert_eq!(named(&view, &s, "Hear the difference").checked, Some(true));
        let req = key(&mut view, &mut s, "Codec: Apple Music", Key::Enter);
        assert!(matches!(req.first(), Some(HostRequest::ContextMenu { .. })));
        assert_eq!(
            named(&view, &s, "True peak decoded").spoken(),
            "True peak decoded, no reading"
        );
    }

    fn readings(true_peak: f64, integrated: f64, correlation: f32) -> Readings {
        Readings {
            true_peak,
            integrated,
            target: -14.0,
            correlation,
            codec_overs: None,
        }
    }

    /// Each limit is said once when crossed: the true peak, the target
    /// reached and left, the correlation below zero and back (not around
    /// zero), the codec's overs.
    #[test]
    fn limits_are_said_when_crossed() {
        let mut said = Vec::new();
        let mut limits = Limits::default();
        for r in [
            readings(f64::NEG_INFINITY, f64::NEG_INFINITY, 0.0),
            readings(-6.0, -20.0, 0.5),
            readings(-3.0, -14.5, 0.3),
            readings(-0.5, -14.2, -0.05),
            readings(-0.4, -14.2, -0.3),
            readings(-0.4, -14.1, 0.05),
            readings(-0.4, -12.0, 0.2),
        ] {
            let now = limits.of(&r);
            said.extend(limits.crossed(&now, &r));
            limits = now;
        }
        assert_eq!(
            said,
            [
                "Integrated loudness 6.0 LU under the target",
                "Integrated loudness on target: −14.5 LUFS",
                "True peak over −1.0 dBTP: −0.5 dBTP",
                "Correlation below zero: −0.30, out of phase",
                // (Not at +0.05: between the thresholds.)
                "Integrated loudness 2.0 LU over the target",
                "Correlation positive again",
            ]
        );
        let r = Readings {
            codec_overs: Some(3),
            ..readings(-6.0, -12.0, 0.5)
        };
        let now = limits.of(&r);
        assert_eq!(
            limits.crossed(&now, &r),
            ["Codec: decoded peaks over full scale"]
        );
    }

    /// Off, nothing is said; the first look only looks.
    #[test]
    fn limits_are_said_only_on_request() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let mut view = ToolsView::new(Theme::default());
        assert!(view.access_news(&s).is_empty());
        key(&mut view, &mut s, "Announce limits", Key::Enter);
        assert!(view.access_news(&s).is_empty(), "nothing measured");
    }
}
