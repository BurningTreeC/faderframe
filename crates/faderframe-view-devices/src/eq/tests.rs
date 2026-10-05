#![allow(clippy::unwrap_used)]

use super::bars::{BottomItem, TopItem};
use super::matching::{Match, Reference};
use super::panel::PanelItem;
use super::*;
use faderframe_core::builtin;
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::eq::{band_id, band_index};
use faderframe_project::PluginRef;
use faderframe_ui_canvas::{
    EventCx, HostRequest, Key, MenuItem, Modifiers, PointerButton, RecordingPainter,
};

const SIZE: Size = Size::new(1180.0, 654.0);

/// The demo with an EQ on its first audio track; the plugin's id.
fn session() -> (Session, PluginInstanceId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let track = s.project().tracks[0].id;
    s.dispatch(Action::InsertPlugin {
        track,
        index: 0,
        plugin: PluginRef::builtin(builtin::EQ, "EQ"),
    })
    .unwrap();
    let plugin = s.project().tracks[0].inserts[0].id;
    assert!(s.plugin_tap(plugin).is_some(), "the engine hosts it");
    (s, plugin)
}

fn run(view: &mut EqView, s: &mut Session, ev: ViewEvent) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    requests
}

fn with(mods: Modifiers) -> Modifiers {
    mods
}

fn down_mods(pos: Point, button: PointerButton, clicks: u32, modifiers: Modifiers) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button,
        modifiers,
        clicks,
    }
}

fn down(pos: Point, button: PointerButton, clicks: u32) -> ViewEvent {
    down_mods(pos, button, clicks, Modifiers::NONE)
}

fn drag(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn up(pos: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

fn key(key: Key, modifiers: Modifiers) -> ViewEvent {
    ViewEvent::Key { key, modifiers }
}

const CTRL: Modifiers = Modifiers {
    shift: false,
    ctrl: true,
    alt: false,
    meta: false,
};
const ALT: Modifiers = Modifiers {
    shift: false,
    ctrl: false,
    alt: true,
    meta: false,
};
const ALT_SHIFT: Modifiers = Modifiers {
    shift: true,
    ctrl: false,
    alt: true,
    meta: false,
};

fn band(s: &Session, plugin: PluginInstanceId, b: usize) -> BandParams {
    BandParams::read(&s.plugin_tap(plugin).unwrap().params, b)
}

/// Click a whole press and release at `pos`.
fn click(
    view: &mut EqView,
    s: &mut Session,
    pos: Point,
    clicks: u32,
    mods: Modifiers,
) -> Vec<HostRequest<Action>> {
    let mut r = run(
        view,
        s,
        down_mods(pos, PointerButton::Primary, clicks, mods),
    );
    r.extend(run(view, s, up(pos)));
    r
}

fn menu(reqs: Vec<HostRequest<Action>>) -> Vec<MenuItem<Action>> {
    reqs.into_iter()
        .find_map(|r| match r {
            HostRequest::ContextMenu { items, .. } => Some(items),
            _ => None,
        })
        .expect("a menu")
}

fn pick(s: &mut Session, items: &[MenuItem<Action>], label: &str) {
    let item = items.iter().find(|i| i.label == label).unwrap_or_else(|| {
        panic!(
            "no '{label}' in {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        )
    });
    s.dispatch(item.action.clone().unwrap()).unwrap();
}

/// Where a band's node is drawn.
fn node(view: &EqView, s: &Session, plugin: PluginInstanceId, b: usize) -> Point {
    let l = view.layout(SIZE, s);
    let tap = s.plugin_tap(plugin).unwrap();
    view.node_at(s, &l, &tap, &band(s, plugin, b))
}

fn paint(view: &mut EqView, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    assert!(p.balanced_clips());
    p
}

#[test]
fn bands_are_added_dragged_shaped_and_removed_on_the_display() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    paint(&mut view, &s);
    // A click on the empty display: a bell where it was, +x dB.
    let at = Point::new(590.0, 200.0);
    click(&mut view, &mut s, at, 1, Modifiers::NONE);
    let b = band(&s, plugin, 0);
    assert!(b.enabled && b.used, "{b:?}");
    assert_eq!(b.kind, BandType::Bell);
    assert!((300.0..3_000.0).contains(&b.freq), "{}", b.freq);
    assert!(b.gain > 1.0, "above the 0 dB line: {}", b.gain);
    assert_eq!(s.history().undo_label(), Some("Add EQ Band"));
    // Drag its node right and down: one step.
    let at = node(&view, &s, plugin, 0);
    run(&mut view, &mut s, down(at, PointerButton::Primary, 1));
    run(
        &mut view,
        &mut s,
        drag(Point::new(at.x + 100.0, at.y + 60.0)),
    );
    run(&mut view, &mut s, up(Point::new(at.x + 100.0, at.y + 60.0)));
    let moved = band(&s, plugin, 0);
    assert!(moved.freq > b.freq * 1.5, "{} → {}", b.freq, moved.freq);
    assert!(moved.gain < b.gain - 2.0);
    assert_eq!(s.history().undo_label(), Some("EQ Band"));
    s.dispatch(Action::Undo).unwrap();
    assert!(
        (band(&s, plugin, 0).freq - b.freq).abs() < 1.0,
        "one undo step"
    );
    // A click on a node without moving changes nothing.
    let at = node(&view, &s, plugin, 0);
    let label = s.history().undo_label().map(str::to_string);
    click(&mut view, &mut s, at, 1, Modifiers::NONE);
    assert_eq!(s.history().undo_label().map(str::to_string), label);
    // The wheel on the node narrows it.
    let q = band(&s, plugin, 0).q;
    run(
        &mut view,
        &mut s,
        ViewEvent::Scroll {
            pos: at,
            dx: 0.0,
            dy: -1.0,
            modifiers: Modifiers::NONE,
            precise: false,
        },
    );
    assert!(band(&s, plugin, 0).q > q);
    // With Alt the wheel makes it dynamic.
    run(
        &mut view,
        &mut s,
        ViewEvent::Scroll {
            pos: at,
            dx: 0.0,
            dy: -1.0,
            modifiers: ALT,
            precise: false,
        },
    );
    assert!(band(&s, plugin, 0).dynamic());
    // Right-click: the band's menu, the shapes first.
    let items = menu(run(
        &mut view,
        &mut s,
        down(at, PointerButton::Secondary, 1),
    ));
    assert_eq!(items[0].label, "Bell");
    pick(&mut s, &items, "Low Shelf");
    assert_eq!(band(&s, plugin, 0).kind, BandType::LowShelf);
    // Alt-click: bypassed (still shown), and back.
    let at = node(&view, &s, plugin, 0);
    click(&mut view, &mut s, at, 1, ALT);
    let b = band(&s, plugin, 0);
    assert!(b.used && !b.enabled, "bypassed");
    click(&mut view, &mut s, at, 1, ALT);
    assert!(band(&s, plugin, 0).enabled);
    // Ctrl+Alt-click: the next shape.
    click(&mut view, &mut s, at, 1, Modifiers { ctrl: true, ..ALT });
    assert_eq!(band(&s, plugin, 0).kind, BandType::LowCut);
    // Delete removes the selected band.
    run(&mut view, &mut s, key(Key::Delete, Modifiers::NONE));
    assert!(!band(&s, plugin, 0).used);
    // At the far left a double-click adds a low cut, at the far right a
    // high cut, low down a notch (with nothing selected, so the band
    // controls are out of the way).
    let l = view.layout(SIZE, &s);
    click(
        &mut view,
        &mut s,
        Point::new(l.graph.x + 10.0, 300.0),
        2,
        Modifiers::NONE,
    );
    assert_eq!(band(&s, plugin, 0).kind, BandType::LowCut);
    click(
        &mut view,
        &mut s,
        Point::new(l.graph.right() - 10.0, 300.0),
        2,
        Modifiers::NONE,
    );
    assert_eq!(band(&s, plugin, 1).kind, BandType::HighCut);
    run(&mut view, &mut s, key(Key::Escape, Modifiers::NONE));
    click(
        &mut view,
        &mut s,
        Point::new(l.graph.x + l.graph.w * 0.5, l.graph.bottom() - 10.0),
        2,
        Modifiers::NONE,
    );
    assert_eq!(band(&s, plugin, 2).kind, BandType::Notch);
    let p = paint(&mut view, &s);
    assert!(
        p.texts().iter().any(|t| t.starts_with("Band 3")),
        "the band panel shows it"
    );
}

#[test]
fn a_double_click_on_a_node_types_its_frequency() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    click(
        &mut view,
        &mut s,
        Point::new(590.0, 200.0),
        2,
        Modifiers::NONE,
    );
    let at = node(&view, &s, plugin, 0);
    let reqs = run(&mut view, &mut s, down(at, PointerButton::Primary, 2));
    let commit = reqs
        .into_iter()
        .find_map(|r| match r {
            HostRequest::TextInput { commit, .. } => Some(commit),
            _ => None,
        })
        .expect("a text entry");
    run(&mut view, &mut s, up(at));
    s.dispatch(commit("A4").unwrap()).unwrap();
    assert!((band(&s, plugin, 0).freq - 440.0).abs() < 0.01);
    s.dispatch(commit("2.5k").unwrap()).unwrap();
    assert!((band(&s, plugin, 0).freq - 2_500.0).abs() < 0.01);
    assert!(commit("nonsense").is_none());
}

#[test]
fn several_bands_move_together_and_a_rectangle_selects() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    click(
        &mut view,
        &mut s,
        Point::new(400.0, 220.0),
        2,
        Modifiers::NONE,
    );
    click(
        &mut view,
        &mut s,
        Point::new(800.0, 260.0),
        2,
        Modifiers::NONE,
    );
    // Ctrl-click the first: both selected.
    let a = node(&view, &s, plugin, 0);
    click(&mut view, &mut s, a, 1, CTRL);
    assert_eq!(view.selected.len(), 2);
    let (f0, f1) = (band(&s, plugin, 0).freq, band(&s, plugin, 1).freq);
    run(&mut view, &mut s, down(a, PointerButton::Primary, 1));
    run(&mut view, &mut s, drag(Point::new(a.x + 80.0, a.y)));
    run(&mut view, &mut s, up(Point::new(a.x + 80.0, a.y)));
    let r0 = band(&s, plugin, 0).freq / f0;
    let r1 = band(&s, plugin, 1).freq / f1;
    assert!(r0 > 1.3 && (r0 - r1).abs() < 1e-3, "{r0} {r1}");
    // A click on the background deselects; a rectangle selects again.
    let l = view.layout(SIZE, &s);
    let empty = Point::new(l.graph.x + 40.0, l.graph.y + 20.0);
    click(&mut view, &mut s, empty, 1, Modifiers::NONE);
    assert!(view.selected.is_empty());
    run(&mut view, &mut s, down(empty, PointerButton::Primary, 1));
    let corner = Point::new(l.graph.right() - 20.0, l.graph.bottom() - 20.0);
    run(&mut view, &mut s, drag(corner));
    run(&mut view, &mut s, up(corner));
    assert_eq!(view.selected.len(), 2, "both inside the rectangle");
    // Ctrl+C, Ctrl+V: copies.
    run(&mut view, &mut s, key(Key::Char('c'), CTRL));
    run(&mut view, &mut s, key(Key::Char('v'), CTRL));
    assert!(band(&s, plugin, 2).used && band(&s, plugin, 3).used);
    assert_eq!(band(&s, plugin, 2).freq, band(&s, plugin, 0).freq);
}

#[test]
fn the_band_controls_edit_every_selected_band() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    click(
        &mut view,
        &mut s,
        Point::new(590.0, 260.0),
        2,
        Modifiers::NONE,
    );
    paint(&mut view, &s);
    let l = view.layout(SIZE, &s);
    let tap = s.plugin_tap(plugin).unwrap();
    let (r, _, p) = view.panel_rect(&l, &s, &tap).expect("the controls");
    let items = view.panel_items(&r, &p);
    let at = |item: PanelItem| {
        items
            .iter()
            .find(|(i, _)| *i == item)
            .map(|(_, r)| r.center())
            .unwrap()
    };
    // The gain knob, dragged up.
    let gain0 = band(&s, plugin, 0).gain;
    let knob = at(PanelItem::Knob(Field::Gain));
    run(&mut view, &mut s, down(knob, PointerButton::Primary, 1));
    run(&mut view, &mut s, drag(Point::new(knob.x, knob.y - 40.0)));
    run(&mut view, &mut s, up(Point::new(knob.x, knob.y - 40.0)));
    assert!(band(&s, plugin, 0).gain > gain0 + 5.0);
    assert_eq!(
        f64::from(tap.params.get(band_index(0, Field::Gain))),
        band(&s, plugin, 0).gain
    );
    // The ring makes it dynamic; ">>" opens its own settings, where the
    // sidechain keys it.
    let (c, _, ring) = EqView::gain_knob(
        &items
            .iter()
            .find(|(i, _)| *i == PanelItem::Knob(Field::Gain))
            .unwrap()
            .1,
    );
    let on_ring = Point::new(c.x, c.y - ring);
    run(&mut view, &mut s, down(on_ring, PointerButton::Primary, 1));
    run(
        &mut view,
        &mut s,
        drag(Point::new(on_ring.x, on_ring.y + 30.0)),
    );
    run(
        &mut view,
        &mut s,
        up(Point::new(on_ring.x, on_ring.y + 30.0)),
    );
    assert!(
        band(&s, plugin, 0).range < -2.0,
        "{}",
        band(&s, plugin, 0).range
    );
    paint(&mut view, &s);
    let items = {
        let (r, _, p) = view.panel_rect(&l, &s, &tap).unwrap();
        view.panel_items(&r, &p)
    };
    let at = |item: PanelItem| {
        items
            .iter()
            .find(|(i, _)| *i == item)
            .map(|(_, r)| r.center())
            .unwrap()
    };
    click(&mut view, &mut s, at(PanelItem::Expand), 1, Modifiers::NONE);
    assert!(band(&s, plugin, 0).custom);
    paint(&mut view, &s);
    let items = {
        let (r, _, p) = view.panel_rect(&l, &s, &tap).unwrap();
        view.panel_items(&r, &p)
    };
    let at = |item: PanelItem| {
        items
            .iter()
            .find(|(i, _)| *i == item)
            .map(|(_, r)| (r.center(), *r))
            .unwrap()
    };
    click(&mut view, &mut s, at(PanelItem::Key).0, 1, Modifiers::NONE);
    assert!(band(&s, plugin, 0).keyed_externally());
    // The threshold slider: its far right is "auto", further left a level.
    let (_, track) = at(PanelItem::Threshold);
    click(
        &mut view,
        &mut s,
        Point::new(track.x + track.w * 0.25, track.center().y),
        1,
        Modifiers::NONE,
    );
    let thr = band(&s, plugin, 0).threshold;
    assert!(thr < -50.0 && thr > -70.0, "{thr}");
    click(
        &mut view,
        &mut s,
        Point::new(track.right() - 1.0, track.center().y),
        1,
        Modifiers::NONE,
    );
    assert!(band(&s, plugin, 0).auto_threshold());
    // Free triggering starts round the band.
    click(
        &mut view,
        &mut s,
        at(PanelItem::Trigger).0,
        1,
        Modifiers::NONE,
    );
    let b = band(&s, plugin, 0);
    assert!(
        b.free && b.trigger_low > 100.0 && b.trigger_high < 20_000.0,
        "{b:?}"
    );
    // The shape menu.
    let items = {
        let (r, _, p) = view.panel_rect(&l, &s, &tap).unwrap();
        view.panel_items(&r, &p)
    };
    let shape = items
        .iter()
        .find(|(i, _)| *i == PanelItem::Shape)
        .unwrap()
        .1
        .center();
    let m = menu(click(&mut view, &mut s, shape, 1, Modifiers::NONE));
    pick(&mut s, &m, "High Shelf");
    assert_eq!(band(&s, plugin, 0).kind, BandType::HighShelf);
}

#[test]
fn dynamic_and_spectral_bands_are_made_with_alt() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    click(&mut view, &mut s, Point::new(590.0, 180.0), 2, ALT);
    let b = band(&s, plugin, 0);
    assert!(b.dynamic() && b.gain == 0.0 && !b.spectral, "{b:?}");
    click(&mut view, &mut s, Point::new(800.0, 180.0), 2, ALT_SHIFT);
    let b = band(&s, plugin, 1);
    assert!(b.dynamic() && b.is_spectral(), "{b:?}");
    paint(&mut view, &s);
}

#[test]
fn the_sidechain_source_is_picked_in_the_editor() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    let l = view.layout(SIZE, &s);
    let r = view
        .top_items(&l.top)
        .into_iter()
        .find(|(i, _)| *i == TopItem::Sidechain)
        .unwrap()
        .1;
    let items = menu(click(&mut view, &mut s, r.center(), 1, Modifiers::NONE));
    assert_eq!(items[0].label, "None");
    assert!(items.len() > 1, "other tracks to choose");
    let name = items[1].label.clone();
    pick(&mut s, &items, &name);
    let (_, slot) = s.plugin_slot(plugin).unwrap();
    assert!(slot.sidechain.is_some());
    let p = paint(&mut view, &s);
    assert!(
        p.texts().iter().any(|t| t.contains(&name)),
        "shown in the bar"
    );
}

#[test]
fn the_analyser_menu_sets_the_editors_own_settings() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    let l = view.layout(SIZE, &s);
    let r = view
        .bottom_items(&l.bottom)
        .into_iter()
        .find(|(i, _)| *i == BottomItem::Analyser)
        .unwrap()
        .1;
    let items = menu(click(&mut view, &mut s, r.center(), 1, Modifiers::NONE));
    for label in [
        "Pre-EQ",
        "Post-EQ",
        "External: Side Chain",
        "Freeze",
        "Show Collisions",
        "Resolution: Maximum",
        "Tilt: 3.0 dB/oct",
    ] {
        assert!(items.iter().any(|i| i.label == label), "{label}");
    }
    let undo = s.history().undo_label().map(str::to_string);
    pick(&mut s, &items, "External: Side Chain");
    pick(&mut s, &items, "Resolution: Maximum");
    let st = view.settings(&s);
    assert!(st.external && st.source == Source::Sidechain);
    assert_eq!(st.resolution, 3);
    assert_eq!(
        s.history().undo_label().map(str::to_string),
        undo,
        "not an undo step"
    );
    // Another editor of another EQ keeps its own.
    let other = EqView::new(PluginInstanceId(plugin.0 + 99_999), &Theme::default());
    assert!(!other.settings(&s).external);
    paint(&mut view, &s);
}

#[test]
fn a_sketch_draws_bands_in_one_undo_step() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    let l = view.layout(SIZE, &s);
    let g = l.graph;
    let gains = view.gain_axis(&s);
    // With no bands a drag sketches: flat, a bump of +6 dB round 1 kHz,
    // flat again.
    let axis = view.axis(&s);
    let y0 = gains.y(&g, 0.0);
    let start = Point::new(axis.x(&g, 60.0), y0);
    run(&mut view, &mut s, down(start, PointerButton::Primary, 1));
    let mut f = 60.0;
    while f < 15_000.0 {
        let db = 6.0 * (-((f / 1000.0f64).log2().powi(2)) / 0.5).exp();
        run(
            &mut view,
            &mut s,
            drag(Point::new(axis.x(&g, f), gains.y(&g, db as f32))),
        );
        f *= 1.06;
    }
    run(&mut view, &mut s, up(Point::new(axis.x(&g, 15_000.0), y0)));
    let used: Vec<BandParams> = (0..faderframe_plugin_host::eq::BANDS)
        .map(|b| band(&s, plugin, b))
        .filter(|b| b.used)
        .collect();
    assert_eq!(used.len(), 1, "{used:?}");
    assert_eq!(used[0].kind, BandType::Bell);
    assert!(
        (used[0].freq / 1000.0).log2().abs() < 0.25,
        "{}",
        used[0].freq
    );
    assert!(used[0].gain > 4.0);
    assert_eq!(s.history().undo_label(), Some("EQ Sketch"));
}

#[test]
fn a_and_b_hold_two_settings() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    click(
        &mut view,
        &mut s,
        Point::new(590.0, 200.0),
        2,
        Modifiers::NONE,
    );
    let a_gain = band(&s, plugin, 0).gain;
    let l = view.layout(SIZE, &s);
    let top = |item: TopItem| {
        view.top_items(&l.top)
            .into_iter()
            .find(|(i, _)| *i == item)
            .unwrap()
            .1
            .center()
    };
    let (ta, tb) = (top(TopItem::A), top(TopItem::B));
    click(&mut view, &mut s, tb, 1, Modifiers::NONE);
    // B starts as a copy; change it.
    s.dispatch(Action::Edit(
        faderframe_project::Command::SetPluginParameter {
            track: s.project().tracks[0].id,
            plugin,
            parameter: band_id(0, Field::Gain),
            value: Some(-9.0),
        },
    ))
    .unwrap();
    click(&mut view, &mut s, ta, 1, Modifiers::NONE);
    assert!((band(&s, plugin, 0).gain - a_gain).abs() < 1e-6);
    click(&mut view, &mut s, tb, 1, Modifiers::NONE);
    assert!((band(&s, plugin, 0).gain + 9.0).abs() < 1e-6);
}

#[test]
fn the_piano_puts_bands_on_notes() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    s.dispatch(view.view_action(key::PIANO, 1.0)).unwrap();
    click(
        &mut view,
        &mut s,
        Point::new(590.0, 200.0),
        2,
        Modifiers::NONE,
    );
    let f = band(&s, plugin, 0).freq;
    let l = view.layout(SIZE, &s);
    let x = view.axis(&s).x(&l.graph, f);
    click(
        &mut view,
        &mut s,
        Point::new(x, l.axis.center().y),
        1,
        Modifiers::NONE,
    );
    let n = geometry::note_of(band(&s, plugin, 0).freq);
    assert!((n - n.round()).abs() < 1e-4, "{n}");
    let p = paint(&mut view, &s);
    assert!(
        p.texts().iter().any(|t| t.starts_with('C') && t.len() <= 3),
        "octave names"
    );
}

#[test]
fn the_instance_list_shows_every_eq_and_picks_the_external_one() {
    let (mut s, plugin) = session();
    // A second EQ on another track.
    let other_track = s.project().tracks[1].id;
    s.dispatch(Action::InsertPlugin {
        track: other_track,
        index: 0,
        plugin: PluginRef::builtin(builtin::EQ, "EQ"),
    })
    .unwrap();
    let other = s.project().tracks[1].inserts[0].id;
    let mut view = EqView::new(plugin, &Theme::default());
    let l = view.layout(SIZE, &s);
    let r = view
        .bottom_items(&l.bottom)
        .into_iter()
        .find(|(i, _)| *i == BottomItem::Instances)
        .unwrap()
        .1;
    click(&mut view, &mut s, r.center(), 1, Modifiers::NONE);
    assert!(view.instances.is_some());
    let p = paint(&mut view, &s);
    assert!(p.texts().contains(&"INSTANCES"));
    assert!(p.texts().contains(&"this EQ"));
    // Its "Reference" button: the other EQ becomes the external spectrum.
    let mut found = None;
    for y in (0..SIZE.h as i32).step_by(4) {
        for x in (0..SIZE.w as i32).step_by(8) {
            let at = Point::new(x as f32, y as f32);
            if view.hit(at, SIZE, &s) == Some(Hit::Instances(instances::Hit::Reference(other))) {
                found = Some(at);
            }
        }
    }
    let at = found.expect("a Reference button for the other EQ");
    click(&mut view, &mut s, at, 1, Modifiers::NONE);
    let st = view.settings(&s);
    assert!(st.external && st.source == Source::Instance(other));
    run(&mut view, &mut s, key(Key::Escape, Modifiers::NONE));
    assert!(view.instances.is_none());
}

#[test]
fn eq_match_proposes_bands_from_the_difference() {
    let (mut s, plugin) = session();
    let tap = s.plugin_tap(plugin).unwrap();
    // The input is noise, the reference the same noise with its highs
    // brighter (a first difference adds treble).
    let mut seed = 1u32;
    let mut noise = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5
    };
    let mut m = Match::new(48_000);
    let mut last = 0.0f32;
    for _ in 0..80 {
        let x: Vec<f32> = (0..4_096).map(|_| noise() * 0.3).collect();
        let bright: Vec<f32> = x
            .iter()
            .map(|v| {
                let y = v + 1.5 * (v - last);
                last = *v;
                y
            })
            .collect();
        tap.input.push(&x, &x);
        tap.sidechain.push(&bright, &bright);
        m.update(&s, &tap, plugin);
    }
    assert_eq!(Reference::of(&s, plugin), Reference::Sidechain);
    assert!(
        m.ready(),
        "{} {}",
        m.input.averaged_seconds(),
        m.reference.averaged_seconds()
    );
    m.compute(24);
    let bands = m.result.clone().unwrap();
    assert!(!bands.is_empty());
    // More treble: what the bands do at 10 kHz is well over what they do
    // at 200 Hz.
    let at = |f: f64| -> f64 {
        bands
            .iter()
            .map(|b| faderframe_plugin_host::eq::design::analog_db(b, f))
            .sum()
    };
    assert!(
        at(10_000.0) > at(200.0) + 4.0,
        "{} {}",
        at(10_000.0),
        at(200.0)
    );
    // Applied through the editor: one undo step.
    let mut view = EqView::new(plugin, &Theme::default());
    view.matching = Some(m);
    let l = view.layout(SIZE, &s);
    let r = Match::rect(&l);
    let apply = view
        .matching
        .as_ref()
        .unwrap()
        .items(&r)
        .into_iter()
        .find(|(h, _)| *h == matching::Hit::Apply)
        .unwrap()
        .1
        .center();
    paint(&mut view, &s);
    click(&mut view, &mut s, apply, 1, Modifiers::NONE);
    assert!(view.matching.is_none());
    assert_eq!(s.history().undo_label(), Some("EQ Match"));
    assert!(band(&s, plugin, 0).used);
}

#[test]
fn every_part_paints() {
    let (mut s, plugin) = session();
    let mut view = EqView::new(plugin, &Theme::default());
    // Bands of every shape, one selected with its custom dynamics open.
    for (i, kind) in BandType::MENU.iter().enumerate() {
        let x = 120.0 + i as f32 * 90.0;
        click(&mut view, &mut s, Point::new(x, 200.0), 2, Modifiers::NONE);
        let b = view.focus.unwrap();
        s.dispatch(
            view.action(&s, band_id(b, Field::Type), kind.index() as f64)
                .unwrap(),
        )
        .unwrap();
    }
    let b = view.focus.unwrap();
    for (f, v) in [
        (Field::Type, 0.0),
        (Field::Range, -6.0),
        (Field::Dynamics, 1.0),
        (Field::Spectral, 1.0),
        (Field::Trigger, 1.0),
    ] {
        s.dispatch(view.action(&s, band_id(b, f), v).unwrap())
            .unwrap();
    }
    view.output_open = true;
    s.dispatch(view.view_actions(&[(key::EXTERNAL, 1.0), (key::PIANO, 1.0)]))
        .unwrap();
    for theme in Theme::all() {
        view.set_theme(&theme);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &theme);
        assert!(p.balanced_clips(), "{}", theme.id);
        assert!(p.texts().contains(&"OUTPUT"));
        assert!(p.texts().iter().any(|t| t.contains("THRESHOLD")));
    }
    // And the overlays.
    view.matching = Some(Match::new(48_000));
    view.instances = Some(instances::List::new());
    paint(&mut view, &s);
    let _ = with(Modifiers::NONE);
}

#[test]
fn a_peak_of_the_spectrum_is_grabbed_into_a_bell() {
    let (mut s, plugin) = session();
    let tap = s.plugin_tap(plugin).unwrap();
    let mut view = EqView::new(plugin, &Theme::default());
    // The output: a loud 500 Hz tone over quiet noise.
    let mut seed = 7u32;
    let feed = |seed: &mut u32| {
        let v: Vec<f32> = (0..16_384)
            .map(|i| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((*seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.002;
                0.5 * (std::f32::consts::TAU * 500.0 * i as f32 / 48_000.0).sin() + noise
            })
            .collect();
        tap.output.push(&v, &v);
    };
    feed(&mut seed);
    paint(&mut view, &s);
    // Rest on the spectrum at its peak.
    let l = view.layout(SIZE, &s);
    let st = view.settings(&s);
    let x = view.axis(&s).x(&l.graph, 500.0);
    let level =
        view.analyser.as_ref().unwrap().post.curve(&[500.0])[0] + st.tilt * (0.5f64).log2() as f32;
    let at = Point::new(x, geometry::analyser_y(&l.graph, level, st.range));
    run(
        &mut view,
        &mut s,
        ViewEvent::PointerMove {
            pos: at,
            modifiers: Modifiers::NONE,
            dragging: false,
        },
    );
    std::thread::sleep(std::time::Duration::from_millis(1_000));
    feed(&mut seed);
    paint(&mut view, &s);
    let peaks = view.grab.clone().expect("spectrum grab is on");
    let (f, d) = peaks
        .iter()
        .copied()
        .min_by(|a, b| {
            (a.0 / 500.0)
                .ln()
                .abs()
                .total_cmp(&(b.0 / 500.0).ln().abs())
        })
        .unwrap();
    assert!((f / 500.0).log2().abs() < 0.1, "{f}");
    // Grab it and pull it down: a bell there, cutting.
    let peak = Point::new(
        view.axis(&s).x(&l.graph, f),
        geometry::analyser_y(&l.graph, d, st.range),
    );
    assert_eq!(view.hit(peak, SIZE, &s), Some(Hit::Peak(f, d)));
    run(&mut view, &mut s, down(peak, PointerButton::Primary, 1));
    run(&mut view, &mut s, drag(Point::new(peak.x, peak.y + 80.0)));
    run(&mut view, &mut s, up(Point::new(peak.x, peak.y + 80.0)));
    let b = band(&s, plugin, 0);
    assert!(b.used && b.kind == BandType::Bell, "{b:?}");
    assert!(
        (b.freq / 500.0).log2().abs() < 0.1 && b.gain < -3.0,
        "{b:?}"
    );
    assert_eq!(s.history().undo_label(), Some("Spectrum Grab"));
    assert!(view.grab.is_none(), "it ends with the drag");
}

#[test]
fn mono_display_uses_one_meter_reading_for_both_bars() {
    for layout in [
        faderframe_core::ChannelLayout::Mono,
        faderframe_core::ChannelLayout::Stereo,
    ] {
        let (mut s, plugin) = session();
        let track = s.plugin_slot(plugin).unwrap().0.id;
        s.dispatch(Action::Edit(faderframe_project::Command::SetTrackLayout {
            track,
            layout,
        }))
        .unwrap();
        let tap = s.plugin_tap(plugin).unwrap();
        // One active channel, as a mono meter publishes. A stereo track
        // with a silent right side must still show independent bars.
        tap.meter_out.publish(0, 0.5, 0.125);
        let mut view = EqView::new(plugin, &Theme::default());
        view.paint_all(&mut RecordingPainter::new(), SIZE, &s);
        assert!(view.meter[0] > -7.0);
        if layout == faderframe_core::ChannelLayout::Mono {
            assert_eq!(view.meter[0], view.meter[1]);
            view.paint_all(&mut RecordingPainter::new(), SIZE, &s);
            assert_eq!(view.meter[0], view.meter[1], "decay is shared too");
        } else {
            assert!(view.meter[1] < -100.0);
        }
    }
}
