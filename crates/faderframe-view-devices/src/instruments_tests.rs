#![allow(clippy::unwrap_used)]
//! The Wavetable's, the FM's and the Granular's faces.

use crate::fm::FmFace;
use crate::granular::GranularFace;
use crate::kit::{Face, Panel};
use crate::poly::Page;
use crate::wavetable::WavetableFace;
use faderframe_core::{ParameterId, PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, PluginTarget, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, Modifiers, Point, PointerButton, RecordingPainter, Rect,
    Size, Theme, ViewEvent,
};

/// An instrument track playing `id`; the plugin.
fn session(id: &str) -> (Session, PluginInstanceId) {
    let mut s = Session::new(Project::new("Faces", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.place_plugin(t, PluginTarget::Instrument, PluginRef::builtin(id, id))
        .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    assert!(s.plugin_tap(plugin).is_some());
    (s, plugin)
}

fn size(id: &str) -> Size {
    let (w, h) = crate::editor_size(id).unwrap();
    Size::new(w as f32, (h - 46) as f32)
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

fn press(at: Point, button: PointerButton) -> ViewEvent {
    ViewEvent::PointerDown {
        pos: at,
        button,
        modifiers: Modifiers::NONE,
        clicks: 1,
    }
}

fn release(at: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos: at,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

fn drag_to(at: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos: at,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

/// Every control inside a section, none over another or over the display.
fn check_panel(what: &str, panel: &Panel) {
    let display = panel.display.unwrap();
    for c in &panel.controls {
        let r = c.rect;
        assert!(
            panel.sections.iter().any(|s| r.x >= s.rect.x - 0.5
                && r.right() <= s.rect.right() + 0.5
                && r.y >= s.rect.y
                && r.bottom() <= s.rect.bottom() + 0.5),
            "{what}: {} ({r:?}) outside every section",
            c.label
        );
        assert!(
            r.y >= display.bottom(),
            "{what}: {} over the display",
            c.label
        );
    }
    for (i, a) in panel.controls.iter().enumerate() {
        for b in &panel.controls[i + 1..] {
            let (p, q) = (a.rect, b.rect);
            let apart =
                p.right() <= q.x || q.right() <= p.x || p.bottom() <= q.y || q.bottom() <= p.y;
            assert!(apart, "{what}: {} and {} overlap", a.label, b.label);
        }
    }
}

/// Both pages of each instrument (and each of the FM's operators) hold
/// their controls inside their sections without overlap, at the editor's
/// size; together they have a control for every parameter.
#[test]
fn the_instruments_pages_hold_a_control_for_every_parameter() {
    let theme = Theme::default();
    for id in [builtin::WAVETABLE, builtin::FM, builtin::GRANULAR] {
        let at = size(id);
        let mut panels: Vec<(String, Panel)> = Vec::new();
        for page in [Page::Sound, Page::Modulation] {
            match id {
                builtin::WAVETABLE => {
                    let mut f = WavetableFace::new(&theme);
                    f.page = page;
                    assert!(at.w >= f.min_size().w && at.h >= f.min_size().h);
                    panels.push((format!("{id} {page:?}"), f.panel(at)));
                }
                builtin::FM => {
                    for op in 0..6 {
                        let mut f = FmFace::new(&theme);
                        f.page = page;
                        f.op = op;
                        panels.push((format!("{id} {page:?} op {op}"), f.panel(at)));
                    }
                }
                _ => {
                    let mut f = GranularFace::new(&theme);
                    f.page = page;
                    panels.push((format!("{id} {page:?}"), f.panel(at)));
                }
            }
        }
        for (what, p) in &panels {
            check_panel(what, p);
        }
        for p in faderframe_plugin_host::presets::parameters(id) {
            // The FM's free matrix is drawn as a grid in the display.
            if id == builtin::FM && p.id.0 >= faderframe_plugin_host::devices::fm::id::MATRIX {
                continue;
            }
            assert!(
                panels
                    .iter()
                    .any(|(_, panel)| panel.controls.iter().any(|c| c.id == p.id)),
                "{id}: no control for {}",
                p.name
            );
        }
    }
}

/// Each face paints in every theme (its boxes' titles there), and the tabs
/// switch its page.
#[test]
fn the_instruments_faces_paint_and_switch_pages() {
    for (id, wants) in [
        (builtin::WAVETABLE, ["OSCILLATOR 1", "OSC 1"]),
        (builtin::FM, ["ALGORITHM 1", "OPERATOR 1"]),
        (builtin::GRANULAR, ["SAMPLE", "GRAINS"]),
    ] {
        let (mut s, plugin) = session(id);
        let at = size(id);
        for th in Theme::all() {
            let mut view = crate::editor_for(id, plugin, &th).unwrap();
            let mut p = RecordingPainter::new();
            view.paint(&mut p, at, &s, &th);
            assert!(p.balanced_clips(), "{id}: {}", th.name);
            for want in wants {
                assert!(p.texts().contains(&want), "{id} in {}: no {want}", th.name);
            }
        }
        // The modulation tab: the matrix shows.
        let theme = Theme::default();
        let mut view = crate::editor_for(id, plugin, &theme).unwrap();
        let display = WavetableFace::new(&theme).panel(at).display.unwrap();
        let (tab, _) = crate::poly::tabs(display)[1];
        run(
            view.as_mut(),
            &mut s,
            press(tab.center(), PointerButton::Primary),
            at,
        );
        let mut p = RecordingPainter::new();
        view.paint(&mut p, at, &s, &theme);
        assert!(p.texts().contains(&"MATRIX"), "{id}: the modulation page");
    }
}

/// The FM's bank: Import DX7… asks for a file; a voice clicked loads as
/// one undo step; an operator clicked in the algorithm shows on the deck.
#[test]
fn the_fm_face_loads_dx7_voices_and_picks_operators() {
    let (mut s, plugin) = session(builtin::FM);
    let theme = Theme::default();
    let at = size(builtin::FM);
    let mut view = crate::editor_for(builtin::FM, plugin, &theme).unwrap();
    let display = FmFace::new(&theme).panel(at).display.unwrap();
    let r = crate::poly::content(display);
    let w = (r.w - 16.0) / 3.0;
    let bank = Rect::new(r.x + 2.0 * (w + 8.0), r.y, w, r.h);
    // The button asks for a .syx file.
    let button = Point::new(bank.right() - 50.0, bank.y + 12.0);
    let requests = run(
        view.as_mut(),
        &mut s,
        press(button, PointerButton::Primary),
        at,
    );
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, HostRequest::ChooseFiles { .. }))
    );
    // A bank read into the session; its third voice clicked.
    let dir = std::env::temp_dir().join(format!("ff-fm-face-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut dump = vec![0xF0, 0x43, 0x00, 0x09, 0x20, 0x00];
    for v in 0..32u8 {
        let mut d = [0u8; 128];
        for block in 0..6 {
            d[block * 17 + 14] = if block == 5 { 99 } else { 60 };
            d[block * 17 + 15] = 2;
            d[block * 17 + 4] = 99;
        }
        d[106..110].copy_from_slice(&[50; 4]);
        d[110] = v;
        d[117] = 24;
        d[118..128].copy_from_slice(format!("VOICE {v:<4}").as_bytes());
        dump.extend_from_slice(&d);
    }
    dump.extend_from_slice(&[0, 0xF7]);
    std::fs::write(dir.join("bank.syx"), &dump).unwrap();
    s.dispatch(Action::ImportDx7 {
        plugin,
        path: dir.join("bank.syx"),
    })
    .unwrap();
    let steps = s.history_steps().0.len();
    let h = ((bank.h - 34.0) / 16.0).max(6.0);
    let third = Point::new(bank.x + 30.0, bank.y + 28.0 + 2.5 * h);
    run(
        view.as_mut(),
        &mut s,
        press(third, PointerButton::Primary),
        at,
    );
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    assert_eq!(s.dx7_bank(plugin).unwrap().loaded, Some(2));
    let algorithm = s
        .plugin_parameter_value(
            plugin,
            ParameterId(faderframe_plugin_host::devices::fm::id::ALGORITHM),
        )
        .unwrap();
    assert_eq!(algorithm, 2.0, "the voice's algorithm");
    let mut p = RecordingPainter::new();
    view.paint(&mut p, at, &s, &theme);
    assert!(p.texts().iter().any(|t| t.contains("VOICE 2")));
    // An operator clicked in the drawing is the deck's.
    let algorithm_box = Rect::new(r.x, r.y, w, r.h);
    let a = &faderframe_plugin_host::devices::fm::algorithms::ALGORITHMS[2];
    let sixth = FmFace::op_boxes(algorithm_box, a)[5].center();
    run(
        view.as_mut(),
        &mut s,
        press(sixth, PointerButton::Primary),
        at,
    );
    run(view.as_mut(), &mut s, release(sixth), at);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, at, &s, &theme);
    assert!(p.texts().contains(&"OPERATOR 6"), "operator 6 on the deck");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Wavetable's oscillator box: Load… asks for a file (and switches the
/// oscillator to its own table when one is picked), right-click offers the
/// frame sizes.
#[test]
fn the_wavetable_face_loads_tables_and_sets_their_frame_size() {
    let (mut s, plugin) = session(builtin::WAVETABLE);
    let theme = Theme::default();
    let at = size(builtin::WAVETABLE);
    let mut view = crate::editor_for(builtin::WAVETABLE, plugin, &theme).unwrap();
    let display = WavetableFace::new(&theme).panel(at).display.unwrap();
    let r = crate::poly::content(display);
    let w = (r.w - 16.0) / 3.0;
    let osc2 = Rect::new(r.x + w + 8.0, r.y, w, r.h);
    let requests = run(
        view.as_mut(),
        &mut s,
        press(
            Point::new(osc2.right() - 40.0, osc2.y + 12.0),
            PointerButton::Primary,
        ),
        at,
    );
    let Some(HostRequest::ChooseFiles { commit, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ChooseFiles { .. }))
    else {
        panic!("a file chooser");
    };
    let Some(Action::Several(actions)) = commit(vec!["/x/table.wav".into()]) else {
        panic!("load and switch");
    };
    assert!(matches!(
        actions[0],
        Action::LoadDeviceSamples { slot: 1, .. }
    ));
    let requests = run(
        view.as_mut(),
        &mut s,
        press(osc2.center(), PointerButton::Secondary),
        at,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = requests
        .into_iter()
        .find(|r| matches!(r, HostRequest::ContextMenu { .. }))
    else {
        panic!("a menu");
    };
    let item = items
        .into_iter()
        .find(|i| i.label == "Frames of 1024 Samples")
        .unwrap();
    s.dispatch(item.action.unwrap()).unwrap();
    assert_eq!(s.wavetable_cycle(plugin, 1), 1024);
}

/// A drag across the Granular's sample moves the position, one undo step.
#[test]
fn a_drag_in_the_granulars_sample_moves_the_position() {
    use faderframe_plugin_host::devices::granular::id;
    let (mut s, plugin) = session(builtin::GRANULAR);
    let theme = Theme::default();
    let at = size(builtin::GRANULAR);
    let mut view = crate::editor_for(builtin::GRANULAR, plugin, &theme).unwrap();
    let display = GranularFace::new(&theme).panel(at).display.unwrap();
    let r = crate::poly::content(display);
    let wave_w = (r.w - 8.0) * 0.75 - 20.0;
    let y = r.y + r.h / 2.0;
    let steps = s.history_steps().0.len();
    run(
        view.as_mut(),
        &mut s,
        press(
            Point::new(r.x + 10.0 + wave_w * 0.2, y),
            PointerButton::Primary,
        ),
        at,
    );
    run(
        view.as_mut(),
        &mut s,
        drag_to(Point::new(r.x + 10.0 + wave_w * 0.8, y)),
        at,
    );
    run(
        view.as_mut(),
        &mut s,
        release(Point::new(r.x + 10.0 + wave_w * 0.8, y)),
        at,
    );
    let v = s
        .plugin_parameter_value(plugin, ParameterId(id::POSITION))
        .unwrap();
    assert!((v - 0.8).abs() < 0.02, "{v}");
    assert_eq!(s.history_steps().0.len(), steps + 1);
}
