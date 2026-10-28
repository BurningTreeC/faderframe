#![allow(clippy::unwrap_used)]
//! Auditioning a take (issue #28): the folder plays one take whole instead
//! of its comp, playback starting at the folder when stopped; a change to
//! the folder, or another project, ends it. The project is not edited.

use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, Take, TakeFolder};
use faderframe_session::{Action, Session};

/// The demo with its "Bass" clip a folder of three takes, the third in
/// the comp.
fn session_with_takes() -> (Session, ClipId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let clip = s.project().track(bass).unwrap().clips[0];
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    let mut f = TakeFolder::new(a.length);
    for i in 0..3 {
        f.add_take(Take {
            name: format!("Take {}", i + 1),
            source: a.source,
            source_offset: a.source_offset,
            start: 0,
            end: a.length,
            gain_db: 0.0,
            rating: 0,
        });
    }
    f.use_take(2);
    s.edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    (s, clip)
}

#[test]
fn a_take_is_auditioned_until_its_folder_changes() {
    let (mut s, clip) = session_with_takes();
    let revision = s.project().clip(clip).unwrap().clone();
    let undo_steps = s.history_steps().0.len();
    s.dispatch(Action::AuditionTake {
        clip,
        take: Some(0),
    })
    .unwrap();
    assert_eq!(s.auditioned_take(), Some((clip, 0)));
    assert!(s.transport().playing, "stopped: it plays");
    let start = s.project().clip(clip).unwrap().start;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.tick(0.005);
    }
    assert!(
        s.playhead().quarters() > start.quarters() - 1e-3,
        "from the folder: {:?} vs {start:?}",
        s.playhead()
    );
    assert_eq!(s.project().clip(clip).unwrap(), &revision, "not edited");
    assert_eq!(s.history_steps().0.len(), undo_steps, "no undo step");
    // Another take; the comp again.
    s.dispatch(Action::AuditionTake {
        clip,
        take: Some(1),
    })
    .unwrap();
    assert_eq!(s.auditioned_take(), Some((clip, 1)));
    s.dispatch(Action::AuditionTake { clip, take: None })
        .unwrap();
    assert_eq!(s.auditioned_take(), None);
    // No such take: nothing.
    s.dispatch(Action::AuditionTake {
        clip,
        take: Some(9),
    })
    .unwrap();
    assert_eq!(s.auditioned_take(), None);
    // A comp edit ends it (the comp is what the user wants to hear then).
    s.dispatch(Action::AuditionTake {
        clip,
        take: Some(1),
    })
    .unwrap();
    let mut f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    f.use_take(0);
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip,
        start,
        content: Box::new(ClipContent::Takes(f)),
    }))
    .unwrap();
    assert_eq!(s.auditioned_take(), None, "the comp changed");
    // Undo changes the folder back: an audition then ends too.
    s.dispatch(Action::AuditionTake {
        clip,
        take: Some(2),
    })
    .unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.auditioned_take(), None);
    // Another project: none.
    s.dispatch(Action::AuditionTake {
        clip,
        take: Some(2),
    })
    .unwrap();
    s.new_project(false).unwrap();
    assert_eq!(s.auditioned_take(), None);
}
