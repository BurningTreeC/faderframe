//! Stem separation from the arranger: the clip menu's Stems and the track
//! menu's Separate Stems, and what choosing one does (a stand-in
//! separator: the model is not in the repository).

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::separate::{Stem, StemSeparator, StemTarget};
use std::sync::Arc;

/// Hands back half of the mix as each source.
struct Halves;

impl StemSeparator for Halves {
    fn sample_rate(&self) -> u32 {
        44_100
    }

    fn sources(&self) -> Vec<String> {
        ["bass", "drums", "vocals", "other"]
            .map(String::from)
            .to_vec()
    }

    fn separate(
        &self,
        left: &[f32],
        right: &[f32],
        _progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Vec<[Vec<f32>; 2]>, String> {
        let half = || {
            [
                left.iter().map(|v| v * 0.5).collect(),
                right.iter().map(|v| v * 0.5).collect(),
            ]
        };
        Ok(vec![half(), half(), half(), half()])
    }
}

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

fn track(s: &Session, name: &str) -> Track {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .clone()
}

#[test]
fn an_audio_clips_menu_extracts_its_vocals() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_stem_separator(Some(Arc::new(Halves)));
    let pad = track(&s, "Pad");
    let clip = s.project().clips_of(pad.id)[0].clone();
    let stems = submenu(
        ArrangerView::clip_menu(&s, &clip, Point::new(0.0, 0.0)),
        "Stems",
    );
    let labels: Vec<&str> = stems.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Separate Stems (Vocals, Drums, Bass, Other)",
            "Extract Vocals",
            "Remove Vocals (Instrumental)",
            "Vocals and Instrumental",
        ]
    );
    let extract = stems.iter().find(|i| i.label == "Extract Vocals").unwrap();
    assert_eq!(
        extract.action,
        Some(Action::SeparateStems {
            target: StemTarget::Clip(clip.id),
            model: None,
            stems: vec![Stem::Vocals],
        })
    );
    let tracks = s.project().tracks.len();
    s.dispatch(extract.action.clone().unwrap()).unwrap();
    s.wait_for_separations();
    assert_eq!(s.project().tracks.len(), tracks + 1);
    let vocals = track(&s, &format!("{} Vocals", clip.name));
    assert_eq!(s.project().clips_of(vocals.id)[0].start, clip.start);
    assert!(s.project().clip(clip.id).unwrap().muted);
}

#[test]
fn an_audio_tracks_menu_separates_it_and_a_midi_clips_does_not() {
    let s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let stems = submenu(
        ArrangerView::track_menu(&s, &drums, Point::new(0.0, 0.0)),
        "Separate Stems",
    );
    assert_eq!(
        stems[0].action,
        Some(Action::SeparateStems {
            target: StemTarget::Track(drums.id),
            model: None,
            stems: Stem::SOURCES.to_vec(),
        })
    );
    let midi = s
        .project()
        .clips
        .values()
        .find(|c| c.as_midi().is_some())
        .unwrap()
        .clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::clip_menu(&s, &midi, Point::new(0.0, 0.0))
    else {
        panic!("a menu")
    };
    assert!(items.iter().all(|i| i.label != "Stems"));
}
