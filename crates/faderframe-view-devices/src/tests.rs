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
