//! Slicing from the arranger: the clip menu's and the track menu's
//! "Slice to Pads or Keys", and what choosing an entry does.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::slicing::{SliceBy, SliceSource, SliceTarget};

fn submenu(menu: HostRequest<Action>, label: &str) -> Vec<MenuItem<Action>> {
    let HostRequest::ContextMenu { items, .. } = menu else {
        panic!("a menu")
    };
    items
        .into_iter()
        .find(|i| i.label == label)
        .map(|i| i.children)
        .unwrap_or_else(|| panic!("no {label}"))
}

#[test]
fn an_audio_clips_menu_slices_it_onto_pads_or_keys() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    let clip = s.project().clips_of(drums)[0].clone();
    let slices = submenu(
        ArrangerView::clip_menu(&s, &clip, Point::new(0.0, 0.0)),
        "Slice to Pads or Keys",
    );
    let labels: Vec<&str> = slices.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels[0], "Slice Clip at Transients to New Drum Sampler");
    assert!(labels.contains(&"Slice Clip on the Grid to New Sampler (a Key Each)"));
    let first = slices[0].action.clone().unwrap();
    let p = s.project();
    assert_eq!(
        first,
        Action::Slice {
            source: SliceSource::Span {
                track: drums,
                start: clip.start,
                end: clip.end(&p.timeline, p.sample_rate),
            },
            by: SliceBy::Transients,
            target: SliceTarget::NewDrums,
            midi: true,
        }
    );
    s.dispatch(first).unwrap();
    assert!(s.slicing());
    assert!(s.wait_for_slicing(std::time::Duration::from_secs(120)));
    assert!(
        s.project().tracks.iter().any(|t| t.name == "Drums Slices"),
        "a new kit plays the slices"
    );
    // MIDI and instrument tracks have nothing to slice.
    let lead = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::track_menu(&s, &lead, Point::new(0.0, 0.0))
    else {
        panic!("a menu")
    };
    assert!(items.iter().all(|i| i.label != "Slice to Pads or Keys"));
}
