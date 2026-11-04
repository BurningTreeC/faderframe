//! ARA from the arranger: an audio clip's menu offers its ARA entries (the
//! session's `ara_menu`; with no ARA plug-in installed, a disabled one
//! saying so — the session's tests open clips with the mock plug-in).

use super::*;
use faderframe_engine::EngineConfig;

#[test]
fn an_audio_clips_menu_offers_ara() {
    let s = Session::demo(EngineConfig::default()).unwrap();
    let pad = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Pad")
        .unwrap()
        .id;
    let clip = s.project().clips_of(pad)[0].clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::clip_menu(&s, &clip, Point::new(0.0, 0.0))
    else {
        panic!("a menu")
    };
    let ara = items
        .iter()
        .find(|i| i.label.contains("ARA"))
        .expect("an ARA entry");
    assert_eq!(ara.label, "Open with ARA (no ARA plug-in installed)");
    assert!(ara.action.is_none(), "disabled");
    // MIDI clips have none.
    let keys = s
        .project()
        .clips
        .values()
        .find(|c| c.as_midi().is_some())
        .unwrap()
        .clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::clip_menu(&s, &keys, Point::new(0.0, 0.0))
    else {
        panic!("a menu")
    };
    assert!(!items.iter().any(|i| i.label.contains("ARA")));
}
