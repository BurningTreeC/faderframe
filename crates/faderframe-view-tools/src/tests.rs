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
