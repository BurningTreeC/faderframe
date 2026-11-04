#![allow(clippy::unwrap_used)]

use crate::program_eq::{ProgramEqView, light_at};
use faderframe_core::{ParameterId, PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::program_eq::param;
use faderframe_project::PluginRef;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, Modifiers, Point, PointerButton, RecordingPainter, Size,
    Theme, ViewEvent,
};

/// The demo with `id` on its first audio track; the plugin's id.
fn session(id: &str, name: &str) -> (Session, PluginInstanceId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let track = s.project().tracks[0].id;
    s.dispatch(Action::InsertPlugin {
        track,
        index: 0,
        plugin: PluginRef::builtin(id, name),
    })
    .unwrap();
    let plugin = s.project().tracks[0].inserts[0].id;
    assert!(s.plugin_tap(plugin).is_some(), "the engine hosts it");
    (s, plugin)
}

fn run(
    view: &mut dyn CanvasView<Session, Action>,
    s: &mut Session,
    ev: ViewEvent,
    size: Size,
) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, size, s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    requests
}

fn down(pos: Point, button: PointerButton, clicks: u32) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button,
        modifiers: Modifiers::NONE,
        clicks,
    }
}

fn drag(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn up(pos: Point, button: PointerButton) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn the_program_eq_panel_turns_switches_and_knobs() {
    let (mut s, plugin) = session(builtin::PROGRAM_EQ, "Program EQ");
    let size = Size::new(1160.0, 356.0);
    let mut view = ProgramEqView::new(plugin, &Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.balanced_clips());
    assert!(p.texts().contains(&"PROGRAM EQ"));
    let value = |s: &Session, i: usize| f64::from(s.plugin_tap(plugin).unwrap().params.get(i));
    // At scale 1 the panel starts 34 below the top.
    let panel = |x: f32, y: f32| Point::new(x, y + 34.0);
    // Drag the low boost knob (330, 88) up by its whole range.
    run(
        &mut view,
        &mut s,
        down(panel(330.0, 88.0), PointerButton::Primary, 1),
        size,
    );
    run(&mut view, &mut s, drag(panel(330.0, 88.0 - 130.0)), size);
    run(
        &mut view,
        &mut s,
        up(panel(330.0, -42.0), PointerButton::Primary),
        size,
    );
    assert!((value(&s, param::LOW_BOOST) - 5.0).abs() < 0.2);
    // Click the "30" engraved round the low frequency selector.
    let at = crate::program_eq::selector_angle(1, 4).to_radians();
    let (x, y) = (400.0 + 54.0 * at.sin(), 234.0 - 54.0 * at.cos());
    run(
        &mut view,
        &mut s,
        down(panel(x, y), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, param::LOW_FREQ), 1.0);
    // A click on the power switch throws it off.
    run(
        &mut view,
        &mut s,
        down(panel(985.0, 234.0), PointerButton::Primary, 1),
        size,
    );
    run(
        &mut view,
        &mut s,
        up(panel(985.0, 234.0), PointerButton::Primary),
        size,
    );
    assert_eq!(value(&s, param::POWER), 0.0);
    // The oversampling strip.
    let r = 1160.0 - 24.0 - 4.0 * 38.0 + 17.0;
    run(
        &mut view,
        &mut s,
        down(Point::new(r, 17.0), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, param::OVERSAMPLING), 0.0);
    // Double-click resets a knob.
    run(
        &mut view,
        &mut s,
        down(panel(330.0, 88.0), PointerButton::Primary, 2),
        size,
    );
    assert_eq!(value(&s, param::LOW_BOOST), 0.0);
    assert!(light_at(0.0, 0.0) > light_at(1160.0, 322.0));
    let _ = ParameterId(0);
}

mod guitar_station {
    use super::*;
    use crate::guitar::GuitarView;
    use faderframe_guitar::pedal::Stomp;
    use faderframe_guitar::voice::Pedal;
    use faderframe_plugin_host::devices::guitar::id;
    use faderframe_ui_canvas::MenuItem;

    const SIZE: Size = Size::new(crate::guitar::PANEL_W, crate::guitar::TOTAL_H);

    /// A panel point at scale 1 (the panel starts below the header).
    fn at(x: f32, y: f32) -> Point {
        Point::new(x, y + crate::guitar::HEADER_H)
    }

    fn value(s: &Session, plugin: PluginInstanceId, id: u32) -> f64 {
        f64::from(s.plugin_tap(plugin).unwrap().params.get(id as usize))
    }

    fn click(view: &mut GuitarView, s: &mut Session, p: Point) -> Vec<HostRequest<Action>> {
        let r = run(view, s, down(p, PointerButton::Primary, 1), SIZE);
        run(view, s, up(p, PointerButton::Primary), SIZE);
        r
    }

    /// The action of a menu's item called `label`.
    fn pick(requests: Vec<HostRequest<Action>>, label: &str) -> Action {
        fn find(items: Vec<MenuItem<Action>>, label: &str) -> Option<Action> {
            items.into_iter().find_map(|i| {
                if i.label == label {
                    i.action
                } else {
                    find(i.children, label)
                }
            })
        }
        let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
            panic!("no menu");
        };
        find(items, label).unwrap_or_else(|| panic!("no {label}"))
    }

    #[test]
    fn pedals_are_added_switched_moved_and_removed_on_the_line() {
        let (mut s, plugin) = session(builtin::GUITAR_STATION, "Guitar Station");
        let mut view = GuitarView::new(plugin, &Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.balanced_clips());
        assert!(p.texts().contains(&"BRIT 800"));
        assert!(p.texts().contains(&"ADD PEDAL"));
        // Add two pedals through the empty place's menu.
        let add = |view: &mut GuitarView, s: &mut Session, n: usize, name: &str| {
            let r = crate::guitar::layout::pedal_rect(n);
            let menu = click(view, s, at(r.center().x, r.center().y));
            s.dispatch(pick(menu, name)).unwrap();
        };
        add(&mut view, &mut s, 0, "Green 808");
        add(&mut view, &mut s, 1, "Ram Fuzz");
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::Green808).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::STOMP)),
            Stomp::Pedal(Pedal::BigMuff).index() as f64
        );
        // The footswitch of the first.
        let r = crate::guitar::layout::pedal_rect(0);
        click(&mut view, &mut s, at(r.center().x, r.y + 172.0));
        assert_eq!(value(&s, plugin, id::slot(0, id::ON)), 0.0);
        // Its drive knob, dragged up.
        let knob = at(r.x + 8.0 + (r.w - 16.0) / 6.0, r.y + 14.0 + 46.0 - 4.0);
        let before = value(&s, plugin, id::slot(0, id::P_DRIVE));
        run(
            &mut view,
            &mut s,
            down(knob, PointerButton::Primary, 1),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            drag(Point::new(knob.x, knob.y - 60.0)),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            up(Point::new(knob.x, knob.y - 60.0), PointerButton::Primary),
            SIZE,
        );
        assert!(value(&s, plugin, id::slot(0, id::P_DRIVE)) > before + 0.2);
        // Carry the second pedal before the first: its knobs go with it.
        let second = crate::guitar::layout::pedal_rect(1);
        let grab = at(second.x + 20.0, second.y + 150.0);
        run(
            &mut view,
            &mut s,
            down(grab, PointerButton::Primary, 1),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            drag(Point::new(grab.x - 136.0, grab.y)),
            SIZE,
        );
        run(
            &mut view,
            &mut s,
            up(Point::new(grab.x - 136.0, grab.y), PointerButton::Primary),
            SIZE,
        );
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::BigMuff).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::STOMP)),
            Stomp::Pedal(Pedal::Green808).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::ON)),
            0.0,
            "its footswitch moved with it"
        );
        // One undo step brings the order back.
        s.dispatch(Action::Undo).unwrap();
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::Green808).index() as f64
        );
        // Remove the first: the line closes up.
        let menu = run(
            &mut view,
            &mut s,
            down(at(r.center().x, r.y + 137.0), PointerButton::Secondary, 1),
            SIZE,
        );
        s.dispatch(pick(menu, "Remove Pedal")).unwrap();
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::BigMuff).index() as f64
        );
        assert_eq!(value(&s, plugin, id::slot(1, id::STOMP)), 0.0);
        // The amplifier's menu.
        let name = crate::guitar::layout::amp_look_for_tests();
        let menu = click(&mut view, &mut s, at(name.center().x, name.center().y));
        s.dispatch(pick(menu, "American Twin")).unwrap();
        assert_eq!(value(&s, plugin, id::AMP), 8.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.texts().contains(&"AMERICAN TWIN"));
        assert!(p.texts().contains(&"REVERB"), "the Twin's own controls");
        assert!(!p.texts().contains(&"MASTER"), "an AB763 has no master");
    }

    /// The compressors come from the menu's Dynamics group and wear their
    /// boxes' knobs: two each, no tone row.
    #[test]
    fn the_compressors_are_on_the_line_with_their_knobs() {
        let (mut s, plugin) = session(builtin::GUITAR_STATION, "Guitar Station");
        let mut view = GuitarView::new(plugin, &Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        for (n, name) in ["Red Squeeze", "Juicer"].into_iter().enumerate() {
            let r = crate::guitar::layout::pedal_rect(n);
            let menu = click(&mut view, &mut s, at(r.center().x, r.center().y));
            s.dispatch(pick(menu, name)).unwrap();
        }
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::RedSqueeze).index() as f64
        );
        assert_eq!(
            value(&s, plugin, id::slot(1, id::STOMP)),
            Stomp::Pedal(Pedal::Juicer).index() as f64
        );
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.balanced_clips());
        let texts = p.texts();
        for label in [
            "RED SQUEEZE",
            "SENSITIVITY",
            "OUTPUT",
            "JUICER",
            "BIAS",
            "VOLUME",
        ] {
            assert!(texts.contains(&label), "no {label}: {texts:?}");
        }
        assert_eq!(Stomp::Pedal(Pedal::RedSqueeze).family(), "Dynamics");
        assert_eq!(Stomp::Pedal(Pedal::Juicer).family(), "Dynamics");
        assert!(!Stomp::Pedal(Pedal::Juicer).knobs().tones[0].is_some());
    }

    /// The Carbon Echo comes off the stomp menu and wears its box's four
    /// controls: DELAY on the drive knob, MIX on the level, REGEN and the MOD
    /// switch on the tone row.
    #[test]
    fn the_carbon_echo_is_on_the_menu_with_its_four_controls() {
        let (mut s, plugin) = session(builtin::GUITAR_STATION, "Guitar Station");
        let mut view = GuitarView::new(plugin, &Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        let r = crate::guitar::layout::pedal_rect(0);
        let menu = click(&mut view, &mut s, at(r.center().x, r.center().y));
        s.dispatch(pick(menu, "Carbon Echo")).unwrap();
        assert_eq!(
            value(&s, plugin, id::slot(0, id::STOMP)),
            Stomp::Pedal(Pedal::CarbonEcho).index() as f64
        );
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.balanced_clips());
        let texts = p.texts();
        for label in ["DELAY", "MIX", "REGEN", "MOD"] {
            assert!(
                texts.iter().any(|t| t.eq_ignore_ascii_case(label)),
                "no {label} among {texts:?}"
            );
        }
    }

    /// The Tweed Lux and the Seattle 100 show their own panels: the tweed's
    /// one TONE, its other channel's volume and its jacks; the Seattle's
    /// PREAMP, DEPTH, Crunch and Bright and its master.
    #[test]
    fn the_tweed_and_the_seattle_show_their_own_panels() {
        let (mut s, plugin) = session(builtin::GUITAR_STATION, "Guitar Station");
        let mut view = GuitarView::new(plugin, &Theme::default());
        let name = crate::guitar::layout::amp_look_for_tests();
        let menu = click(&mut view, &mut s, at(name.center().x, name.center().y));
        s.dispatch(pick(menu, "Tweed Lux")).unwrap();
        assert_eq!(value(&s, plugin, id::AMP), 20.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        assert!(p.balanced_clips());
        let texts = p.texts();
        for t in [
            "TWEED LUX",
            "VOLUME",
            "TONE",
            "NORMAL VOL",
            "INPUT",
            "Jack 1",
        ] {
            assert!(texts.contains(&t), "{t}: {texts:?}");
        }
        for t in ["MASTER", "PRESENCE", "BASS", "TREBLE"] {
            assert!(!texts.contains(&t), "a 5E3 has no {t}");
        }
        let menu = click(&mut view, &mut s, at(name.center().x, name.center().y));
        s.dispatch(pick(menu, "Seattle 100")).unwrap();
        assert_eq!(value(&s, plugin, id::AMP), 22.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        let texts = p.texts();
        for t in [
            "SEATTLE 100",
            "PREAMP",
            "BASS",
            "MIDDLE",
            "TREBLE",
            "DEPTH",
            "PRESENCE",
            "MASTER",
            "CRUNCH",
            "Crunch",
            "BRIGHT",
        ] {
            assert!(texts.contains(&t), "{t}: {texts:?}");
        }
    }
}

/// Every device control's menu learns a MIDI controller and, once one is
/// mapped, removes it (the Guitar Station's, the stock devices', the EQs').
#[test]
fn a_mapped_control_can_be_unmapped_from_its_menu() {
    use faderframe_automation::AutomationTarget;
    use faderframe_project::{
        Command, MappingMode, MappingTarget, MidiControl, MidiMapping, MidiSource,
    };
    let (mut s, plugin) = session(builtin::GUITAR_STATION, "Guitar Station");
    let (track, _) = s.plugin_owner(plugin).unwrap();
    let target = MappingTarget::Parameter {
        track,
        target: AutomationTarget::PluginParameter {
            plugin,
            parameter: ParameterId(faderframe_plugin_host::devices::guitar::id::DRIVE),
        },
    };
    let labels = |s: &Session| -> Vec<String> {
        crate::kit::learn_items(s, target)
            .into_iter()
            .map(|i| i.label)
            .collect()
    };
    assert_eq!(labels(&s), vec!["MIDI Learn…".to_string()]);
    s.dispatch(Action::Edit(Command::AddMidiMapping {
        index: 0,
        mapping: MidiMapping {
            id: faderframe_core::MidiMappingId(77),
            source: MidiSource {
                port: None,
                channel: 0,
                control: MidiControl::Cc { number: 21 },
            },
            target,
            mode: MappingMode::default(),
        },
    }))
    .unwrap();
    let items = crate::kit::learn_items(&s, target);
    let remove = items
        .iter()
        .find(|i| i.label.starts_with("Remove MIDI Mapping"))
        .expect("a removal once mapped");
    s.dispatch(remove.action.clone().unwrap()).unwrap();
    assert!(s.midi_mappings_for(target).is_empty(), "removed");
}

/// The 76's panel: a ratio button alone lets the others out, Shift-click
/// presses several in (all four, or none), Meter Off switches it off and
/// the attack knob turns down to Off.
#[test]
fn the_76_buttons_go_in_together_as_on_the_hardware() {
    use crate::fet76::Fet76View;
    use faderframe_plugin_host::devices::fet76::{self as fet, id};
    let (mut s, plugin) = session(builtin::COMPRESSOR_76, "76 Compressor");
    let size = Size::new(crate::fet76::PANEL_W, crate::fet76::TOTAL_H);
    let mut view = Fet76View::new(plugin, &Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    assert!(p.balanced_clips());
    assert!(p.texts().contains(&"COMPRESSOR"));
    assert!(
        p.texts().contains(&"OFF"),
        "the attack's Off and the meter's"
    );
    let value =
        |s: &Session, i: u32| f64::from(s.plugin_tap(plugin).unwrap().params.get(i as usize));
    // At scale 1 the panel starts 40 below the top.
    let at = |x: f32, y: f32| Point::new(x, y + 40.0);
    // Button n of a column (from the top).
    let button = |x: f32, n: usize| at(x + 33.0, 40.0 + 48.0 * n as f32 + 20.0);
    let shift = |pos: Point| ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::NONE
        },
        clicks: 1,
    };
    let (ratio_x, meter_x) = (548.0, 638.0);
    // 20 alone.
    run(
        &mut view,
        &mut s,
        down(button(ratio_x, 0), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::RATIO), 8.0);
    // With 4: the outer two.
    run(&mut view, &mut s, shift(button(ratio_x, 3)), size);
    assert_eq!(
        fet::buttons_name(fet::buttons(value(&s, id::RATIO))),
        "4 + 20"
    );
    // All four.
    run(&mut view, &mut s, shift(button(ratio_x, 1)), size);
    run(&mut view, &mut s, shift(button(ratio_x, 2)), size);
    assert_eq!(value(&s, id::RATIO), 15.0);
    // 8 alone again: the others come out.
    run(
        &mut view,
        &mut s,
        down(button(ratio_x, 2), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::RATIO), 2.0);
    // Let it out too: none in.
    run(&mut view, &mut s, shift(button(ratio_x, 2)), size);
    assert_eq!(value(&s, id::RATIO), 0.0);
    // Meter Off: the power switch.
    run(
        &mut view,
        &mut s,
        down(button(meter_x, 3), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::METER), fet::POWER_OFF as f64);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &Theme::default());
    run(
        &mut view,
        &mut s,
        down(button(meter_x, 0), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::METER), 0.0);
    // The attack knob (452, 94) dragged all the way down: Off.
    run(
        &mut view,
        &mut s,
        down(at(452.0, 94.0), PointerButton::Primary, 1),
        size,
    );
    run(&mut view, &mut s, drag(at(452.0, 400.0)), size);
    run(
        &mut view,
        &mut s,
        up(at(452.0, 400.0), PointerButton::Primary),
        size,
    );
    assert!(fet::attack_off(value(&s, id::ATTACK)));
    // The mix slider in the strip: its left end is dry.
    run(
        &mut view,
        &mut s,
        down(Point::new(104.0, 20.0), PointerButton::Primary, 1),
        size,
    );
    run(
        &mut view,
        &mut s,
        up(Point::new(104.0, 20.0), PointerButton::Primary),
        size,
    );
    assert!(value(&s, id::MIX) < 0.01);
}

/// The 2A's panel in every skin: the big knobs turn (and come back to
/// their defaults on a double-click), the toggle flips between Compress and
/// Limit, the selector takes a legend's click or steps round, the strip's
/// switches, mix and R37 trim work, and every control's right-click menu
/// offers its automation and MIDI learn.
#[test]
fn the_2a_panel_turns_its_knobs_and_switches() {
    use crate::leveler::{GAIN_AT, LevelerView, PANEL_W, PEAK_AT, SELECTOR_AT, TOGGLE_AT, TOTAL_H};
    use faderframe_plugin_host::devices::leveler::id;
    let (mut s, plugin) = session(builtin::LEVELER_2A, "2A Leveler");
    let size = Size::new(PANEL_W, TOTAL_H);
    let mut view = LevelerView::new(plugin, &Theme::default());
    for theme in Theme::builtin() {
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &theme);
        assert!(p.balanced_clips(), "{}", theme.id);
        for text in [
            "LEVELER",
            "GAIN",
            "PEAK REDUCTION",
            "COMPRESS",
            "LIMIT",
            "VU",
        ] {
            assert!(p.texts().contains(&text), "{}: {text}", theme.id);
        }
    }
    let value =
        |s: &Session, i: u32| f64::from(s.plugin_tap(plugin).unwrap().params.get(i as usize));
    // At scale 1 the panel starts 44 below the top.
    let at = |p: Point| Point::new(p.x, p.y + 44.0);
    // Gain dragged up: more; a double-click: its default again.
    let gain = value(&s, id::GAIN);
    run(
        &mut view,
        &mut s,
        down(at(GAIN_AT), PointerButton::Primary, 1),
        size,
    );
    run(
        &mut view,
        &mut s,
        drag(at(GAIN_AT.offset(0.0, -60.0))),
        size,
    );
    run(
        &mut view,
        &mut s,
        up(at(GAIN_AT.offset(0.0, -60.0)), PointerButton::Primary),
        size,
    );
    assert!(value(&s, id::GAIN) > gain + 10.0, "{}", value(&s, id::GAIN));
    run(
        &mut view,
        &mut s,
        down(at(GAIN_AT), PointerButton::Primary, 2),
        size,
    );
    assert_eq!(value(&s, id::GAIN), gain);
    // Peak Reduction down half its travel.
    let before = value(&s, id::PEAK_REDUCTION);
    run(
        &mut view,
        &mut s,
        down(at(PEAK_AT), PointerButton::Primary, 1),
        size,
    );
    run(
        &mut view,
        &mut s,
        drag(at(PEAK_AT.offset(0.0, 130.0))),
        size,
    );
    run(
        &mut view,
        &mut s,
        up(at(PEAK_AT.offset(0.0, 130.0)), PointerButton::Primary),
        size,
    );
    let peak = value(&s, id::PEAK_REDUCTION);
    assert!(
        (peak - (before - 50.0)).abs() < 2.0,
        "half the travel: {before} → {peak}"
    );
    // The toggle: Limit, then Compress again.
    assert_eq!(value(&s, id::MODE), 0.0);
    run(
        &mut view,
        &mut s,
        down(at(TOGGLE_AT), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::MODE), 1.0);
    run(
        &mut view,
        &mut s,
        down(at(TOGGLE_AT), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::MODE), 0.0);
    // The selector: +10's legend (up and to the right), then a step round
    // on its knob back to Gain Reduction.
    let ten = Point::new(SELECTOR_AT.x + 30.0, SELECTOR_AT.y - 17.5);
    run(
        &mut view,
        &mut s,
        down(at(ten), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::METER), 2.0);
    run(
        &mut view,
        &mut s,
        down(at(SELECTOR_AT), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::METER), 0.0);
    // The strip: link off, the external sidechain off, mix at its left end
    // (dry), the R37 trim turned up.
    run(
        &mut view,
        &mut s,
        down(Point::new(860.0, 22.0), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::LINK), 0.0);
    run(
        &mut view,
        &mut s,
        down(Point::new(1010.0, 22.0), PointerButton::Primary, 1),
        size,
    );
    assert_eq!(value(&s, id::EXTERNAL), 0.0);
    run(
        &mut view,
        &mut s,
        down(Point::new(112.0, 22.0), PointerButton::Primary, 1),
        size,
    );
    run(
        &mut view,
        &mut s,
        up(Point::new(112.0, 22.0), PointerButton::Primary),
        size,
    );
    assert!(value(&s, id::MIX) < 0.01);
    let trim = Point::new(590.0, 22.0);
    run(
        &mut view,
        &mut s,
        down(trim, PointerButton::Primary, 1),
        size,
    );
    run(&mut view, &mut s, drag(trim.offset(0.0, -100.0)), size);
    run(
        &mut view,
        &mut s,
        up(trim.offset(0.0, -100.0), PointerButton::Primary),
        size,
    );
    assert!(value(&s, id::EMPHASIS) > 0.3, "{}", value(&s, id::EMPHASIS));
    // A knob's menu: its default, its automation, MIDI learn.
    let requests = run(
        &mut view,
        &mut s,
        down(at(PEAK_AT), PointerButton::Secondary, 1),
        size,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests.into_iter().next() else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.iter().any(|l| l.starts_with("Default")),
        "{labels:?}"
    );
    assert!(labels.contains(&"Show Automation"), "{labels:?}");
    assert!(labels.contains(&"MIDI Learn…"), "{labels:?}");
    // The toggle's too (switches are learnable).
    let requests = run(
        &mut view,
        &mut s,
        down(at(TOGGLE_AT), PointerButton::Secondary, 1),
        size,
    );
    assert!(matches!(
        requests.first(),
        Some(HostRequest::ContextMenu { .. })
    ));
}

/// A stock device's panel for screen readers: its sections as groups, its
/// knobs as sliders the arrows move, its switches as toggles.
#[test]
fn a_stock_devices_controls_are_reachable_by_the_keyboard() {
    use faderframe_ui_canvas::{AccessRole, access};
    let (mut s, plugin) = session(builtin::COMPRESSOR, "Compressor");
    let view = crate::editor_for(builtin::COMPRESSOR, plugin, &Theme::default()).unwrap();
    let (w, h) = crate::editor_size(builtin::COMPRESSOR).unwrap_or((900, 420));
    let size = Size::new(w as f32, h as f32);
    let nodes = view.accessible(size, &s);
    let order = access::focus_order(&nodes);
    assert!(
        order.len() >= 5,
        "{:?}",
        order.iter().map(|n| &n.label).collect::<Vec<_>>()
    );
    let threshold = order
        .iter()
        .find(|n| n.label == "Threshold")
        .expect("a Threshold control");
    assert_eq!(threshold.role, AccessRole::Slider);
    let before = threshold.value.as_ref().unwrap().now;
    let text = threshold.value.as_ref().unwrap().text.clone();
    assert!(text.contains("dB"), "{text}");
    s.dispatch(threshold.increment.clone().unwrap()).unwrap();
    let nodes = view.accessible(size, &s);
    let after = access::find_node(&nodes, threshold.id).unwrap();
    assert!(
        after.value.as_ref().unwrap().now > before,
        "the arrow moved it up"
    );
    assert!(
        nodes.iter().any(|n| n.role == AccessRole::Group),
        "sections as groups"
    );
}

/// The Compressor's and the Limiter's decks at their editor sizes: every
/// control inside a section, none over another, and the second
/// revision's controls (hold, channels, upward, oversampling, delta;
/// output, release link) on them.
#[test]
fn the_dynamics_decks_hold_every_control_inside_their_sections() {
    use crate::kit::{Face, Panel};
    use faderframe_plugin_host::devices::{compressor, limiter};
    let theme = Theme::default();
    let check = |id: &str, panel: Panel, min: Size, wanted: &[u32]| {
        let (w, h) = crate::editor_size(id).unwrap();
        assert!(
            w as f32 >= min.w && h as f32 >= min.h,
            "{id}: editor smaller than its face"
        );
        for c in &panel.controls {
            let r = c.rect;
            assert!(
                panel.sections.iter().any(|s| {
                    r.x >= s.rect.x - 0.5
                        && r.right() <= s.rect.right() + 0.5
                        && r.y >= s.rect.y
                        && r.bottom() <= s.rect.bottom() + 0.5
                }),
                "{id}: {} ({:?}) outside every section",
                c.label,
                r
            );
            if let Some(d) = panel.display {
                assert!(r.y >= d.bottom(), "{id}: {} over the display", c.label);
            }
        }
        for (i, a) in panel.controls.iter().enumerate() {
            for b in &panel.controls[i + 1..] {
                let (p, q) = (a.rect, b.rect);
                let apart =
                    p.right() <= q.x || q.right() <= p.x || p.bottom() <= q.y || q.bottom() <= p.y;
                assert!(apart, "{id}: {} and {} overlap", a.label, b.label);
            }
        }
        for want in wanted {
            assert!(
                panel.controls.iter().any(|c| c.id.0 == *want),
                "{id}: no control for parameter {want}"
            );
        }
    };
    let face = crate::compressor::CompressorFace::new(&theme);
    let (w, h) = crate::editor_size(builtin::COMPRESSOR).unwrap();
    check(
        builtin::COMPRESSOR,
        face.panel(Size::new(w as f32, h as f32)),
        face.min_size(),
        &[
            compressor::id::HOLD,
            compressor::id::CHANNELS,
            compressor::id::UPWARD,
            compressor::id::OVERSAMPLE,
            compressor::id::DELTA,
        ],
    );
    // And at its smallest.
    check(
        builtin::COMPRESSOR,
        face.panel(face.min_size()),
        face.min_size(),
        &[],
    );
    let face = crate::limiter::LimiterFace::new(&theme);
    let (w, h) = crate::editor_size(builtin::LIMITER).unwrap();
    check(
        builtin::LIMITER,
        face.panel(Size::new(w as f32, h as f32)),
        face.min_size(),
        &[
            limiter::id::OUTPUT,
            limiter::id::RELEASE_LINK,
            limiter::id::STYLE,
        ],
    );
    check(
        builtin::LIMITER,
        face.panel(face.min_size()),
        face.min_size(),
        &[],
    );
}

/// The Tape Machine's face: its deck holds every parameter's control inside
/// its sections without overlap, at its editor size and its smallest; it
/// paints in every theme (clips balanced, its readouts there); the VU
/// needles settle on the published tape level with a VU's ballistics; a
/// knob's right-click offers its automation and MIDI learn; a drag moves
/// the bias.
#[test]
fn the_tape_machine_face_paints_in_every_theme_and_learns() {
    use crate::kit::{Ctx, Face};
    use faderframe_plugin_host::devices::tape::{id, parameters, value};
    let (mut s, plugin) = session(builtin::TAPE_MACHINE, "Tape Machine");
    let (w, h) = crate::editor_size(builtin::TAPE_MACHINE).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    let theme = Theme::default();
    let face = crate::tape::TapeFace::new(&theme);
    for at in [size, face.min_size()] {
        let panel = face.panel(at);
        for c in &panel.controls {
            let r = c.rect;
            assert!(
                panel.sections.iter().any(|s| r.x >= s.rect.x - 0.5
                    && r.right() <= s.rect.right() + 0.5
                    && r.y >= s.rect.y
                    && r.bottom() <= s.rect.bottom() + 0.5),
                "{} ({r:?}) outside every section at {at:?}",
                c.label
            );
            assert!(
                r.y >= panel.display.unwrap().bottom(),
                "{} over the display",
                c.label
            );
        }
        for (i, a) in panel.controls.iter().enumerate() {
            for b in &panel.controls[i + 1..] {
                let (p, q) = (a.rect, b.rect);
                let apart =
                    p.right() <= q.x || q.right() <= p.x || p.bottom() <= q.y || q.bottom() <= p.y;
                assert!(apart, "{} and {} overlap at {at:?}", a.label, b.label);
            }
        }
        for p in parameters() {
            assert!(
                panel.controls.iter().any(|c| c.id == p.id),
                "no control for {}",
                p.name
            );
        }
    }
    // Every theme.
    for th in Theme::all() {
        let mut view = crate::editor_for(builtin::TAPE_MACHINE, plugin, &th).unwrap();
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &th);
        assert!(p.balanced_clips(), "{}", th.name);
        let texts = p.texts();
        for want in ["RESPONSE", "TAPE  M–H", "BIAS", "WOW & FLUTTER", "VU"] {
            assert!(texts.contains(&want), "{}: no {want}", th.name);
        }
        assert!(
            texts
                .iter()
                .any(|t| t.contains("15 ips") && t.contains("NAB 50 µs") && t.contains("0.04 %")),
            "{}: the machine's line",
            th.name
        );
    }
    // The needles: the tape at its operating level reads 0 VU.
    let tap = s.plugin_tap(plugin).unwrap();
    tap.set_value(value::VU_L, 1.0);
    tap.set_value(value::VU_R, 0.0);
    let mut face = crate::tape::TapeFace::new(&theme);
    let display = face.panel(size).display.unwrap();
    for _ in 0..60 {
        let cx = Ctx {
            model: &s,
            plugin,
            tap: &tap,
            theme: &theme,
            accent: face.accent(),
            dt: 1.0 / 60.0,
            drop: None,
            loading: false,
        };
        face.paint_display(&mut RecordingPainter::new(), display, &cx);
    }
    let zero = faderframe_ui_canvas::controls::vu_deflection(0.0);
    assert!(
        (face.needles[0].pos - zero).abs() < 0.02,
        "{}",
        face.needles[0].pos
    );
    assert!(face.needles[1].pos < 0.05, "{}", face.needles[1].pos);
    // A knob's menu: its automation and MIDI learn.
    let mut view = crate::editor_for(builtin::TAPE_MACHINE, plugin, &theme).unwrap();
    let bias = face
        .panel(size)
        .controls
        .iter()
        .find(|c| c.id == ParameterId(id::BIAS))
        .unwrap()
        .rect;
    let requests = run(
        view.as_mut(),
        &mut s,
        down(bias.center(), PointerButton::Secondary, 1),
        size,
    );
    let Some(items) = requests.into_iter().find_map(|r| match r {
        HostRequest::ContextMenu { items, .. } => Some(items),
        _ => None,
    }) else {
        panic!("a menu");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"Show Automation"), "{labels:?}");
    assert!(labels.contains(&"MIDI Learn…"), "{labels:?}");
    // Drag the bias up.
    let before = f64::from(tap.params.get(id::BIAS as usize));
    run(
        view.as_mut(),
        &mut s,
        down(bias.center(), PointerButton::Primary, 1),
        size,
    );
    let to = Point::new(bias.center().x, bias.center().y - 60.0);
    run(view.as_mut(), &mut s, drag(to), size);
    run(view.as_mut(), &mut s, up(to, PointerButton::Primary), size);
    let after = f64::from(tap.params.get(id::BIAS as usize));
    assert!(after > before + 1.0, "{before} → {after}");
}

/// The restoration devices' faces: every setting has a control inside a
/// section (none over the display, none overlapping) at the window's size
/// and at the smallest; they paint in every theme.
#[test]
fn the_restoration_faces_hold_their_controls_and_paint_in_every_theme() {
    use crate::kit::Face;
    use faderframe_plugin_host::devices::denoise;
    let theme = Theme::default();
    // A device, its name, its face and texts its display shows.
    type Case<'a> = (&'a str, &'a str, Box<dyn Face>, Vec<&'a str>);
    let faces: Vec<Case<'_>> = vec![
        (
            builtin::DENOISE,
            "De-noise",
            Box::new(crate::denoise::DenoiseFace::new(&theme)),
            vec!["LEARN NOISE", "NOISE", "CHARACTER"],
        ),
        (
            builtin::DECLICK,
            "De-click",
            Box::new(crate::declick::DeclickFace::new(&theme)),
            vec!["CLICKS / S", "DETECTION"],
        ),
        (
            builtin::DEHUM,
            "De-hum",
            Box::new(crate::dehum::DehumFace::new(&theme)),
            vec!["FUNDAMENTAL", "NOTCHES"],
        ),
        (
            builtin::DECLIP,
            "De-clip",
            Box::new(crate::declip::DeclipFace::new(&theme)),
            vec!["CLIP LEVEL", "REBUILD"],
        ),
        (
            builtin::DEREVERB,
            "De-reverb",
            Box::new(crate::dereverb::DereverbFace::new(&theme)),
            vec!["ROOM", "early"],
        ),
    ];
    for (id, name, face, wants) in faces {
        let (w, h) = crate::editor_size(id).unwrap();
        let size = Size::new(w as f32, (h - 46) as f32);
        for at in [size, face.min_size()] {
            let panel = face.panel(at);
            let display = panel.display.unwrap();
            for c in &panel.controls {
                let r = c.rect;
                assert!(
                    panel.sections.iter().any(|s| r.x >= s.rect.x - 0.5
                        && r.right() <= s.rect.right() + 0.5
                        && r.y >= s.rect.y
                        && r.bottom() <= s.rect.bottom() + 0.5),
                    "{id}: {} ({r:?}) outside every section at {at:?}",
                    c.label
                );
                assert!(
                    r.y >= display.bottom(),
                    "{id}: {} over the display",
                    c.label
                );
            }
            for (i, a) in panel.controls.iter().enumerate() {
                for b in &panel.controls[i + 1..] {
                    let (p, q) = (a.rect, b.rect);
                    let apart = p.right() <= q.x
                        || q.right() <= p.x
                        || p.bottom() <= q.y
                        || q.bottom() <= p.y;
                    assert!(apart, "{id}: {} and {} overlap at {at:?}", a.label, b.label);
                }
            }
            for p in faderframe_plugin_host::presets::parameters(id) {
                // De-noise's profile is its display's (Learn, Forget).
                if id == builtin::DENOISE && (denoise::hidden(p.id) || p.id.0 == denoise::id::LEARN)
                {
                    continue;
                }
                assert!(
                    panel.controls.iter().any(|c| c.id == p.id),
                    "{id}: no control for {}",
                    p.name
                );
            }
        }
        let (s, plugin) = session(id, name);
        for th in Theme::all() {
            let mut view = crate::editor_for(id, plugin, &th).unwrap();
            let mut p = RecordingPainter::new();
            view.paint(&mut p, size, &s, &th);
            assert!(p.balanced_clips(), "{id}: {}", th.name);
            let texts = p.texts();
            for want in &wants {
                assert!(texts.contains(want), "{id} in {}: no {want}", th.name);
            }
        }
    }
}

/// De-noise's Learn: pressed, it learns; pressed again, what it learned
/// becomes the profile (and the source Learned) in one undo step; Forget
/// goes back to tracking.
#[test]
fn de_noise_learns_a_profile_in_one_undo_step_and_forgets_it() {
    use faderframe_plugin_host::devices::denoise::{BANDS, id, value};
    let (mut s, plugin) = session(builtin::DENOISE, "De-noise");
    let theme = Theme::default();
    let (w, h) = crate::editor_size(builtin::DENOISE).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    let mut view = crate::editor_for(builtin::DENOISE, plugin, &theme).unwrap();
    let display = {
        use crate::kit::Face;
        crate::denoise::DenoiseFace::new(&theme)
            .panel(size)
            .display
            .unwrap()
    };
    let (_, learn, forget) = crate::denoise::DenoiseFace::layout(display);
    let tap = s.plugin_tap(plugin).unwrap();
    let get =
        |s: &mut Session, pid: u32| s.plugin_parameter_value(plugin, ParameterId(pid)).unwrap();
    let click = |view: &mut Box<dyn CanvasView<Session, Action>>, s: &mut Session, at: Point| {
        run(view.as_mut(), s, down(at, PointerButton::Primary, 1), size);
        run(view.as_mut(), s, up(at, PointerButton::Primary), size);
    };
    click(&mut view, &mut s, learn.center());
    assert_eq!(get(&mut s, id::LEARN), 1.0, "learning");
    // What the processor learned (as it publishes it).
    tap.set_value(value::LEARNED, 2.5);
    for b in 0..BANDS {
        tap.set_value(value::NOISE + b, -50.0 + b as f32 * 0.25);
    }
    let steps = s.history_steps().0.len();
    click(&mut view, &mut s, learn.center());
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    assert_eq!(get(&mut s, id::LEARN), 0.0);
    assert_eq!(get(&mut s, id::SOURCE), 1.0);
    assert_eq!(get(&mut s, id::PROFILE_SET), 1.0);
    assert_eq!(get(&mut s, id::profile(8)), -48.0);
    // Undo: still learning, no profile.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(get(&mut s, id::LEARN), 1.0);
    assert_eq!(get(&mut s, id::PROFILE_SET), 0.0);
    s.dispatch(Action::Redo).unwrap();
    // The painted display shows the profile is in use.
    tap.set_value(value::ADAPTIVE, 0.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.texts().contains(&"Learned profile"));
    assert!(p.texts().contains(&"FORGET"));
    click(&mut view, &mut s, forget.center());
    assert_eq!(get(&mut s, id::PROFILE_SET), 0.0);
    assert_eq!(get(&mut s, id::SOURCE), 0.0);
}

/// A face's controls at `at`: each inside a section, none over the
/// display, none overlapping.
fn controls_in_place(id: &str, panel: &crate::kit::Panel, at: Size) {
    let display = panel.display.unwrap();
    for c in &panel.controls {
        let r = c.rect;
        assert!(
            panel.sections.iter().any(|s| r.x >= s.rect.x - 0.5
                && r.right() <= s.rect.right() + 0.5
                && r.y >= s.rect.y
                && r.bottom() <= s.rect.bottom() + 0.5),
            "{id}: {} ({r:?}) outside every section at {at:?}",
            c.label
        );
        assert!(
            r.y >= display.bottom(),
            "{id}: {} over the display",
            c.label
        );
    }
    for (i, a) in panel.controls.iter().enumerate() {
        for b in &panel.controls[i + 1..] {
            let (p, q) = (a.rect, b.rect);
            let apart =
                p.right() <= q.x || q.right() <= p.x || p.bottom() <= q.y || q.bottom() <= p.y;
            assert!(apart, "{id}: {} and {} overlap at {at:?}", a.label, b.label);
        }
    }
}

/// The Convolution Reverb's, Multiband Dynamics' and Vocoder's faces:
/// every setting has a control (the multiband's band settings for the band
/// it shows, every band in turn), inside a section, at the window's size and
/// the smallest; they paint in every theme.
#[test]
fn the_convolution_multiband_and_vocoder_faces_hold_their_controls() {
    use crate::kit::Face;
    let theme = Theme::default();
    type Case<'a> = (&'a str, &'a str, Vec<Box<dyn Face>>, Vec<&'a str>);
    let multiband: Vec<Box<dyn Face>> = (0..faderframe_plugin_host::devices::multiband::BANDS)
        .map(|b| {
            let mut f = crate::multiband::MultibandFace::new(&theme);
            f.selected = b;
            Box::new(f) as Box<dyn Face>
        })
        .collect();
    let cases: Vec<Case<'_>> = vec![
        (
            builtin::CONVOLUTION_REVERB,
            "Convolution Reverb",
            vec![Box::new(crate::convolution::ConvolutionFace::new(&theme))],
            vec!["LOAD…"],
        ),
        (
            builtin::MULTIBAND_DYNAMICS,
            "Multiband Dynamics",
            multiband,
            vec!["THRESHOLD", "SPLIT"],
        ),
        (
            builtin::VOCODER,
            "Vocoder",
            vec![Box::new(crate::vocoder::VocoderFace::new(&theme))],
            vec!["UNVOICED", "The synth: Minor on C3 carries the input"],
        ),
    ];
    for (id, name, faces, wants) in cases {
        let (w, h) = crate::editor_size(id).unwrap();
        let size = Size::new(w as f32, (h - 46) as f32);
        let mut covered = std::collections::HashSet::new();
        for face in &faces {
            for at in [size, face.min_size()] {
                let panel = face.panel(at);
                controls_in_place(id, &panel, at);
                covered.extend(panel.controls.iter().map(|c| c.id));
            }
        }
        for p in faderframe_plugin_host::presets::parameters(id) {
            assert!(covered.contains(&p.id), "{id}: no control for {}", p.name);
        }
        let (s, plugin) = session(id, name);
        for th in Theme::all() {
            let mut view = crate::editor_for(id, plugin, &th).unwrap();
            let mut p = RecordingPainter::new();
            view.paint(&mut p, size, &s, &th);
            assert!(p.balanced_clips(), "{id}: {}", th.name);
            let texts = p.texts();
            for want in &wants {
                assert!(
                    texts.contains(want),
                    "{id} in {}: no {want} ({texts:?})",
                    th.name
                );
            }
        }
    }
}

/// The multiband face: a tab picks the band the deck shows; dragging a
/// crossover moves it as one undo step.
#[test]
fn the_multiband_face_picks_bands_and_drags_crossovers() {
    use crate::kit::Face;
    use faderframe_plugin_host::devices::multiband::id;
    let (mut s, plugin) = session(builtin::MULTIBAND_DYNAMICS, "Multiband Dynamics");
    let theme = Theme::default();
    let (w, h) = crate::editor_size(builtin::MULTIBAND_DYNAMICS).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    let mut view = crate::editor_for(builtin::MULTIBAND_DYNAMICS, plugin, &theme).unwrap();
    let display = crate::multiband::MultibandFace::new(&theme)
        .panel(size)
        .display
        .unwrap();
    let (tabs, spec) = crate::multiband::MultibandFace::layout(display);
    // Band 3's tab: the deck's band section is band 3's.
    let tab = crate::multiband::MultibandFace::tab(tabs, 2).center();
    run(
        view.as_mut(),
        &mut s,
        down(tab, PointerButton::Primary, 1),
        size,
    );
    run(view.as_mut(), &mut s, up(tab, PointerButton::Primary), size);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, &s, &theme);
    assert!(p.texts().contains(&"BAND 3"), "{:?}", p.texts());
    // Drag the first crossover (120 Hz) to about 300 Hz.
    let x = crate::kit::Spectrum::x_of(spec, 120.0);
    let from = Point::new(x, spec.center().y);
    let to = Point::new(crate::kit::Spectrum::x_of(spec, 300.0), spec.center().y);
    let steps = s.history_steps().0.len();
    run(
        view.as_mut(),
        &mut s,
        down(from, PointerButton::Primary, 1),
        size,
    );
    run(view.as_mut(), &mut s, drag(to), size);
    run(view.as_mut(), &mut s, up(to, PointerButton::Primary), size);
    let f = s
        .plugin_parameter_value(plugin, ParameterId(id::CROSSOVER))
        .unwrap();
    assert!((f / 300.0 - 1.0).abs() < 0.05, "{f}");
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
}

/// The convolution face: Load asks for a file; Use File chooses the file
/// as the space.
#[test]
fn the_convolution_face_loads_and_uses_a_file() {
    use crate::kit::Face;
    use faderframe_plugin_host::devices::convolution::id;
    let (mut s, plugin) = session(builtin::CONVOLUTION_REVERB, "Convolution Reverb");
    let theme = Theme::default();
    let (w, h) = crate::editor_size(builtin::CONVOLUTION_REVERB).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    let mut view = crate::editor_for(builtin::CONVOLUTION_REVERB, plugin, &theme).unwrap();
    let display = crate::convolution::ConvolutionFace::new(&theme)
        .panel(size)
        .display
        .unwrap();
    let (_, _, load, clear) = crate::convolution::ConvolutionFace::layout(display);
    let requests = run(
        view.as_mut(),
        &mut s,
        down(load.center(), PointerButton::Primary, 1),
        size,
    );
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, HostRequest::ChooseFiles { .. })),
        "a file chooser"
    );
    run(
        view.as_mut(),
        &mut s,
        up(load.center(), PointerButton::Primary),
        size,
    );
    run(
        view.as_mut(),
        &mut s,
        down(clear.center(), PointerButton::Primary, 1),
        size,
    );
    run(
        view.as_mut(),
        &mut s,
        up(clear.center(), PointerButton::Primary),
        size,
    );
    assert_eq!(
        s.plugin_parameter_value(plugin, ParameterId(id::SPACE)),
        Some(0.0)
    );
}

/// Pitch Correct's face: every setting has a control inside a section at
/// the window's size and the smallest, none overlapping; the display
/// draws the sung and played pitch over the notes the target allows, the
/// target note and the correction, in every theme.
#[test]
fn pitch_correct_shows_the_pitch_and_holds_its_controls() {
    use crate::kit::Face;
    use faderframe_plugin_host::devices::pitch_correct::{id, value};
    let theme = Theme::default();
    let face = crate::pitch_correct::PitchCorrectFace::new(&theme);
    let id_ = builtin::PITCH_CORRECT;
    let (w, h) = crate::editor_size(id_).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    for at in [size, face.min_size()] {
        let panel = face.panel(at);
        let display = panel.display.unwrap();
        for c in &panel.controls {
            let r = c.rect;
            assert!(
                panel.sections.iter().any(|s| r.x >= s.rect.x - 0.5
                    && r.right() <= s.rect.right() + 0.5
                    && r.y >= s.rect.y
                    && r.bottom() <= s.rect.bottom() + 0.5),
                "{} ({r:?}) outside every section at {at:?}",
                c.label
            );
            assert!(r.y >= display.bottom(), "{} over the display", c.label);
        }
        for (i, a) in panel.controls.iter().enumerate() {
            for b in &panel.controls[i + 1..] {
                let (p, q) = (a.rect, b.rect);
                let apart =
                    p.right() <= q.x || q.right() <= p.x || p.bottom() <= q.y || q.bottom() <= p.y;
                assert!(apart, "{} and {} overlap at {at:?}", a.label, b.label);
            }
        }
        for p in faderframe_plugin_host::presets::parameters(id_) {
            assert!(
                panel.controls.iter().any(|c| c.id == p.id),
                "no control for {}",
                p.name
            );
        }
    }
    let (s, plugin) = session(id_, "Pitch Correct");
    // What the processor published: A4 sung 23 cents sharp, corrected,
    // in C major.
    let tap = s.plugin_tap(plugin).unwrap();
    tap.set_value(value::INPUT, 69.23);
    tap.set_value(value::TARGET, 69.0);
    tap.set_value(value::OUTPUT, 69.0);
    tap.set_value(value::CORRECTION, -0.23);
    tap.set_value(value::ALLOWED, f32::from(0b1010_1011_0101u16));
    let rate = f64::from(s.sample_rate());
    let lat = faderframe_plugin_host::devices::pitch_correct::latency(&tap.params, rate);
    let latency = format!("latency {:.1} ms", f64::from(lat) / rate * 1000.0);
    for th in Theme::all() {
        let mut view = crate::editor_for(id_, plugin, &th).unwrap();
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &th);
        assert!(p.balanced_clips(), "{}", th.name);
        let texts = p.texts();
        for want in [
            "TARGET NOTE",
            "A4",
            "+23 ¢",
            "−23 ¢",
            "C5",
            latency.as_str(),
        ] {
            assert!(texts.contains(&want), "{}: no {want} in {texts:?}", th.name);
        }
        // B♭ is not in C major: not named.
        assert!(!texts.contains(&"A#4"));
    }
    assert!(faderframe_core::builtin::has_editor(id_));
    let _ = id::MIX;
}

/// The Sampler's waveform menu slices its sample into equal parts (a
/// key each), shows the cuts, takes one away and drags another; a Drum
/// Sampler pad's menu slices its sample onto the free pads.
#[test]
fn the_samplers_slice_their_samples_from_their_faces() {
    use crate::kit::Face;
    use faderframe_plugin_host::devices::sampler::id;
    use faderframe_plugin_host::devices::samples::Contents;
    use faderframe_session::sampling::SampleTarget;
    use faderframe_timeline::MusicalTime;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let start = s.project().clips_of(bass)[0].start;
    s.dispatch(Action::MakeSample {
        track: bass,
        start,
        end: start + MusicalTime::from_quarters(4.0),
        target: SampleTarget::Sampler,
    })
    .unwrap();
    let wait = |s: &mut Session| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !s.sampling().is_empty() || s.slicing() {
            assert!(std::time::Instant::now() < end);
            s.tick(0.016);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        s.wait_for_samples();
    };
    wait(&mut s);
    let track = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass Sample")
        .unwrap()
        .clone();
    let plugin = s.instrument_slot(&track).unwrap().id;
    let theme = Theme::default();
    let (w, h) = crate::editor_size(builtin::SAMPLER).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    let mut view = crate::editor_for(builtin::SAMPLER, plugin, &theme).unwrap();
    let display = crate::sampler::SamplerFace::new(&theme)
        .panel(size)
        .display
        .unwrap();
    let (_, _, wave, _) = crate::sampler::SamplerFace::split(display);
    let menu = |view: &mut Box<dyn CanvasView<Session, Action>>, s: &mut Session, x: f32| {
        let at = Point::new(x, wave.y + wave.h / 2.0);
        let r = run(
            view.as_mut(),
            s,
            down(at, PointerButton::Secondary, 1),
            size,
        );
        run(view.as_mut(), s, up(at, PointerButton::Secondary), size);
        r.into_iter()
            .find_map(|r| match r {
                HostRequest::ContextMenu { items, .. } => Some(items),
                _ => None,
            })
            .expect("a menu")
    };
    let items = menu(&mut view, &mut s, wave.x + wave.w * 0.5);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"Cut a Slice Here"), "{labels:?}");
    assert!(labels.contains(&"Play Slices (a Key Each)"));
    let four = items
        .iter()
        .find(|i| i.label == "Slice into 4 Equal Parts")
        .unwrap();
    s.dispatch(four.action.clone().unwrap()).unwrap();
    wait(&mut s);
    let contents = |s: &Session| s.plugin_tap(plugin).unwrap().assets::<Contents>().unwrap();
    let frames = contents(&s).set.slot(0).unwrap().frames as u64;
    assert_eq!(contents(&s).doc.slices.len(), 3);
    assert_eq!(contents(&s).set.slices.len(), 4, "the processor's slices");
    assert!(s.plugin_parameter_value(plugin, ParameterId(id::SLICES)) == Some(1.0));
    // Painted: the cuts and their numbers.
    let mut p = RecordingPainter::default();
    view.paint(&mut p, size, &s, &theme);
    let texts = p.texts();
    for n in ["1", "2", "3", "4"] {
        assert!(texts.contains(&n), "slice {n} numbered: {texts:?}");
    }
    // A cut is removed from its own menu.
    let cut = wave.x + wave.w * 0.25;
    let items = menu(&mut view, &mut s, cut);
    let remove = items
        .iter()
        .find(|i| i.label == "Remove the Cut Before Slice 2")
        .unwrap();
    s.dispatch(remove.action.clone().unwrap()).unwrap();
    wait(&mut s);
    assert_eq!(contents(&s).doc.slices, vec![frames / 2, 3 * frames / 4]);
    // The middle cut dragged: one step.
    let steps = s.history_steps().0.len();
    let from = Point::new(wave.x + wave.w * 0.5, wave.y + 10.0);
    let to = Point::new(wave.x + wave.w * 0.6, wave.y + 10.0);
    run(
        view.as_mut(),
        &mut s,
        down(from, PointerButton::Primary, 1),
        size,
    );
    run(view.as_mut(), &mut s, drag(to), size);
    run(view.as_mut(), &mut s, up(to, PointerButton::Primary), size);
    wait(&mut s);
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let moved = contents(&s).doc.slices[0];
    assert!(
        (moved as f64 / frames as f64 - 0.6).abs() < 0.01,
        "{moved} of {frames}"
    );
    // A Drum Sampler pad's menu.
    s.dispatch(Action::Slice {
        source: faderframe_session::slicing::SliceSource::Sample { plugin, slot: 0 },
        by: faderframe_session::slicing::SliceBy::Parts(4),
        target: faderframe_session::slicing::SliceTarget::NewDrums,
        midi: false,
    })
    .unwrap();
    wait(&mut s);
    let kit_track = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name.ends_with("Slices"))
        .unwrap()
        .clone();
    let kit = s.instrument_slot(&kit_track).unwrap().id;
    let (w, h) = crate::editor_size(builtin::DRUMS).unwrap();
    let size = Size::new(w as f32, (h - 46) as f32);
    let mut view = crate::editor_for(builtin::DRUMS, kit, &theme).unwrap();
    let display = crate::drums::DrumsFace::new(&theme)
        .panel(size)
        .display
        .unwrap();
    let (pads, ..) = crate::drums::DrumsFace::split(display);
    let r = run(
        view.as_mut(),
        &mut s,
        down(pads[1].center(), PointerButton::Secondary, 1),
        size,
    );
    let items = r
        .into_iter()
        .find_map(|r| match r {
            HostRequest::ContextMenu { items, .. } => Some(items),
            _ => None,
        })
        .expect("a pad's menu");
    assert_eq!(items[0].label, "Slice Pad 2 at Transients to the Free Pads");
}
