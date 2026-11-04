//! Timing alignment from the arranger: an audio clip's menu offers the
//! other tracks as guides and the alignment's settings.

use super::*;
use faderframe_engine::EngineConfig;
use faderframe_session::timing_align::TimingAlignSettings;

fn submenu(menu: HostRequest<Action>, label: &str) -> Option<Vec<MenuItem<Action>>> {
    let HostRequest::ContextMenu { items, .. } = menu else {
        panic!("a menu")
    };
    items
        .into_iter()
        .find(|i| i.label == label)
        .map(|i| i.children)
}

#[test]
fn an_audio_clips_menu_aligns_it_to_another_tracks_audio() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let p = s.project();
    let audio_tracks: Vec<Track> = p
        .tracks
        .iter()
        .filter(|t| p.clips_of(t.id).iter().any(|c| c.as_audio().is_some()))
        .cloned()
        .collect();
    assert!(audio_tracks.len() >= 2, "the demo has two audio tracks");
    let (dub_track, guide) = (&audio_tracks[0], &audio_tracks[1]);
    let clip = p
        .clips_of(dub_track.id)
        .into_iter()
        .find(|c| c.as_audio().is_some())
        .unwrap()
        .clone();
    let align = submenu(
        ArrangerView::clip_menu(&s, &clip, Point::new(0.0, 0.0)),
        "Align Timing to",
    )
    .unwrap();
    let to_guide = align
        .iter()
        .find(|i| i.label == format!("To ‘{}’", guide.name))
        .unwrap();
    assert_eq!(
        to_guide.action,
        Some(Action::AlignTiming {
            guide: guide.id,
            dubs: vec![clip.id],
            tightness: TimingAlignSettings::default().tightness,
        })
    );
    // Its own track is no guide.
    assert!(
        !align
            .iter()
            .any(|i| i.label == format!("To ‘{}’", dub_track.name))
    );
    // The settings: tightness (Medium chosen), consonants, pitch.
    let medium = align.iter().find(|i| i.label == "Medium").unwrap();
    assert_eq!(medium.checked, Some(true));
    let tight = align.iter().find(|i| i.label == "Tight").unwrap();
    assert_eq!(tight.checked, Some(false));
    assert!(
        align
            .iter()
            .any(|i| i.label == "Protect Consonants" && i.checked == Some(true))
    );
    let pitch = align.iter().find(|i| i.label == "Align Pitch Too").unwrap();
    assert_eq!(pitch.checked, Some(false));
    s.dispatch(pitch.action.clone().unwrap()).unwrap();
    assert!(s.timing_align_settings().pitch > 0.0);
    // A MIDI clip has none.
    let midi = s
        .project()
        .clips
        .values()
        .find(|c| c.as_midi().is_some() && !s.project().is_launcher_clip(c.id))
        .unwrap()
        .clone();
    assert!(
        submenu(
            ArrangerView::clip_menu(&s, &midi, Point::new(0.0, 0.0)),
            "Align Timing to"
        )
        .is_none()
    );
}
