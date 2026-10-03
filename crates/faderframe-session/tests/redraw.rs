//! Redrawing audio samples with the Pencil.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::ClipContent;
use faderframe_session::{Action, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;

#[test]
fn redrawing_copies_the_source_for_that_clip_only_and_undoes() {
    let dir = std::env::temp_dir().join(format!("ff-redraw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.save_as(&dir.join("Redraw.ffproj")).unwrap();
    let clip = s
        .project()
        .clips
        .values()
        .find(|c| c.name == "Pluck Arp")
        .unwrap()
        .clone();
    let audio = clip.as_audio().unwrap().clone();
    // Split: both halves share the source; only the redrawn one changes.
    s.dispatch(Action::SelectClips {
        clips: vec![clip.id],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(4.0),
    )))
    .unwrap();
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    let twin = s
        .project()
        .clips
        .values()
        .find(|c| c.id != clip.id && c.track == clip.track)
        .unwrap()
        .id;
    let before = s.source_frames(audio.source, 1000, 8).unwrap();
    let drawn = [0.5f32, 0.25, 0.0, -0.25];
    s.dispatch(Action::RedrawAudio {
        clip: clip.id,
        channel: None,
        start: 1002,
        samples: drawn.to_vec(),
    })
    .unwrap();
    // Load the new file.
    s.tick(0.016);
    let new = s
        .project()
        .clip(clip.id)
        .unwrap()
        .as_audio()
        .unwrap()
        .source;
    assert_ne!(new, audio.source);
    let after = s.source_frames(new, 1000, 8).unwrap();
    assert_eq!(&after[0][..2], &before[0][..2]);
    assert_eq!(&after[0][2..6], &drawn);
    assert_eq!(&after[0][6..], &before[0][6..]);
    let twin_source = match &s.project().clip(twin).unwrap().content {
        ClipContent::Audio(a) => a.source,
        _ => panic!(),
    };
    assert_eq!(twin_source, audio.source);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .clip(clip.id)
            .unwrap()
            .as_audio()
            .unwrap()
            .source,
        audio.source
    );
    let _ = std::fs::remove_dir_all(&dir);
}
