//! The undo history as a tree in the session: an edit after undoing keeps
//! the steps undone on a branch, any step is reached in one action (the
//! engine synced once), branches by number, and any step auditioned beside
//! the mix — bit for bit what going there renders.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::read_wav;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, History};
use faderframe_session::compare::{CompareOp, Comparing, compare_render_settings};
use faderframe_session::{Action, Session, render};
use std::time::Duration;

fn track(s: &Session, name: &str) -> faderframe_core::TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn volume(s: &mut Session, track: faderframe_core::TrackId, db: f32) -> u64 {
    s.dispatch(Action::Edit(Command::SetTrackVolume { track, db }))
        .unwrap();
    s.history().last_step().unwrap()
}

#[test]
fn an_edit_after_undoing_keeps_a_branch_and_any_step_is_one_action_away() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let a = volume(&mut s, bass, -4.0);
    let b = volume(&mut s, bass, -5.0);
    let c = volume(&mut s, bass, -6.0);
    s.dispatch(Action::HistoryTo(1)).unwrap();
    let d = volume(&mut s, bass, -9.0);
    // The steps undone are kept, on branch 2's side.
    let rows = s.history_tree();
    let shown: Vec<(u64, usize, bool)> = rows
        .iter()
        .map(|r| (r.step.id, r.indent, r.starts_branch))
        .collect();
    assert_eq!(
        shown,
        vec![
            (History::ROOT, 0, false),
            (a, 0, false),
            (d, 1, true),
            (b, 0, false),
            (c, 0, false),
        ]
    );
    assert_eq!(rows[0].step.label, "Project opened");
    assert!(rows[2].step.current);
    assert_eq!(s.history_current(), d);
    assert_eq!(s.history_branch_of(d), Some(2));
    // To the end of the first line in one action; undo and redo follow it.
    s.dispatch(Action::HistoryToNode(c)).unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -6.0);
    assert_eq!(s.history_steps().0.len(), 3);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -5.0);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.history_current(), c);
    // A branch by number: its newest step.
    s.dispatch(Action::HistoryBranch(2)).unwrap();
    assert_eq!(s.project().track(bass).unwrap().volume_db, -9.0);
    s.dispatch(Action::HistoryBranch(1)).unwrap();
    assert_eq!(s.history_current(), c);
    assert!(s.dispatch(Action::HistoryBranch(3)).is_err());
    assert!(s.dispatch(Action::HistoryToNode(999)).is_err());
    // And back to the start.
    s.dispatch(Action::HistoryToNode(History::ROOT)).unwrap();
    assert_eq!(s.history_steps().0.len(), 0);
    assert_ne!(s.project().track(bass).unwrap().volume_db, -4.0);
    let step = s.history_step(History::ROOT).unwrap();
    assert!(step.current && step.label == "Project opened");
}

#[test]
fn a_step_on_another_branch_is_auditioned_as_going_there_renders_it() {
    let dir = std::env::temp_dir().join(format!("ff-undo-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let drums = s.project().tracks[0].id;
    let meter = s.project().timeline.meter.clone();
    s.selection.range = Some(faderframe_session::editing::EditRange::new(
        meter.bar_start(4),
        meter.bar_start(5),
    ));
    let range = s.project().loop_range;
    s.dispatch(Action::Edit(Command::SetLoop {
        range,
        enabled: false,
    }))
    .unwrap();
    let other = volume(&mut s, bass, -20.0);
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: drums,
        on: true,
    }))
    .unwrap();
    let before = s.project().clone();
    s.dispatch(Action::AuditionNode(other)).unwrap();
    assert!(s.wait_for_compare(Duration::from_secs(300)));
    let info = s.audition_info().unwrap();
    assert_eq!(info.error, None);
    assert_eq!((info.node, info.done), (other, 2));
    assert_eq!(info.label, "Change Volume");
    assert_eq!(
        s.compare_status().unwrap().what,
        Comparing::History { node: other }
    );
    assert_eq!(s.project(), &before, "nothing moved");
    // The present is not auditioned.
    let now = s.history_current();
    assert!(s.dispatch(Action::AuditionNode(now)).is_err());
    let then = read_wav(&info.path).unwrap();
    // Going there renders the same, bit for bit.
    s.dispatch(Action::HistoryToNode(other)).unwrap();
    let there = s.project().clone();
    let out = dir.join("there.wav");
    render::start(
        there.clone(),
        compare_render_settings(&there, info.span, info.rate, out.clone()),
    )
    .unwrap()
    .join()
    .unwrap();
    let really = read_wav(&out).unwrap();
    assert_eq!(then.channels, really.channels);
    assert!(then.channels[0].iter().any(|v| v.abs() > 1e-3));
    s.dispatch(Action::Compare(CompareOp::Off)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
