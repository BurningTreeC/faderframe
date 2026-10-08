//! Folder tracks: they hold tracks, open and close, keep their tracks
//! when removed, and sum into a bus on request.
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, OutputRouting, TrackKind};
use faderframe_session::{Action, SelectMode, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// The demo without the folder it comes with (its tracks stay).
fn demo() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let folder = track(&s, "Arpeggios");
    s.dispatch(Action::Edit(Command::RemoveTrack { track: folder }))
        .unwrap();
    s
}

fn order(s: &Session) -> Vec<String> {
    s.project()
        .folder_order()
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[test]
fn a_folder_holds_the_selected_tracks_and_keeps_them_when_removed() {
    let mut s = demo();
    let (bass, pad) = (track(&s, "Bass"), track(&s, "Pad"));
    s.dispatch(Action::NewFolder {
        tracks: vec![pad, bass],
    })
    .unwrap();
    let folder = track(&s, "Folder 1");
    let f = s.project().track(folder).unwrap();
    assert_eq!(f.kind, TrackKind::Folder);
    for t in [bass, pad] {
        assert_eq!(s.project().track(t).unwrap().folder, Some(folder));
    }
    // Shown right under it, in project order, where the first one was.
    let names = order(&s);
    let at = names.iter().position(|n| n == "Folder 1").unwrap();
    assert_eq!(names[at + 1..at + 3], ["Bass", "Pad"]);
    assert_eq!(s.folder_contents(folder), [bass, pad]);
    // Closed: its tracks are hidden in the editors.
    assert!(s.folder_open(folder));
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    assert!(!s.track_shown(s.project().track(bass).unwrap()));
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    // A folder in the folder; it cannot go into itself.
    s.dispatch(Action::NewFolder { tracks: vec![pad] }).unwrap();
    let inner = track(&s, "Folder 2");
    assert_eq!(s.project().track(inner).unwrap().folder, Some(folder));
    assert!(
        s.project()
            .in_folder(s.project().track(pad).unwrap(), folder)
    );
    assert!(
        s.dispatch(Action::Edit(Command::SetTrackFolder {
            track: folder,
            folder: Some(inner),
        }))
        .is_err()
    );
    // Out of the inner folder: a level up, into the outer one.
    s.dispatch(Action::MoveToFolder {
        tracks: vec![pad],
        folder: None,
    })
    .unwrap();
    assert_eq!(s.project().track(pad).unwrap().folder, Some(folder));
    // Removing the outer folder keeps its tracks (one step).
    s.dispatch(Action::Edit(Command::RemoveTrack { track: folder }))
        .unwrap();
    assert!(s.project().track(folder).is_none());
    for t in [bass, pad, inner] {
        assert_eq!(s.project().track(t).unwrap().folder, None, "{t}");
    }
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().folder, Some(folder));
    assert_eq!(s.project().track(inner).unwrap().folder, Some(folder));
}

#[test]
fn a_folder_sums_into_a_bus_on_request() {
    let mut s = demo();
    let (pluck, pad, lead) = (
        track(&s, "Pluck"),
        track(&s, "Pad"),
        track(&s, "Lead Synth"),
    );
    s.selection
        .select_tracks(&[pluck, pad, lead], SelectMode::Replace);
    let choices = s.add_track_choices();
    let (_, new, _) = choices
        .iter()
        .find(|c| c.0 == "Folder with the Selected Tracks")
        .unwrap();
    s.dispatch(new.clone()).unwrap();
    let folder = track(&s, "Folder 1");
    s.dispatch(Action::SumFolder(folder)).unwrap();
    let bus = track(&s, "Folder 1 Bus");
    let b = s.project().track(bus).unwrap();
    assert_eq!(b.kind, TrackKind::Bus);
    assert_eq!(b.folder, Some(folder), "the bus is in the folder");
    for t in [pluck, pad, lead] {
        assert_eq!(
            s.project().track(t).unwrap().output,
            OutputRouting::Track { track: bus }
        );
    }
    // The bus goes to the master; it shows last in the folder.
    assert_eq!(b.output, OutputRouting::Master);
    let names = order(&s);
    let at = names.iter().position(|n| n == "Folder 1").unwrap();
    assert_eq!(names[at + 4], "Folder 1 Bus");
}

#[test]
fn the_track_menu_offers_folders() {
    let mut s = demo();
    let (bass, pad) = (track(&s, "Bass"), track(&s, "Pad"));
    s.dispatch(Action::NewFolder { tracks: vec![bass] })
        .unwrap();
    let labels: Vec<String> = s.folder_choices(pad).into_iter().map(|c| c.0).collect();
    assert_eq!(
        labels,
        ["New Folder with the Track", "Move into ‘Folder 1’"]
    );
    let labels: Vec<String> = s.folder_choices(bass).into_iter().map(|c| c.0).collect();
    assert_eq!(
        labels,
        ["New Folder with the Track", "Move out of ‘Folder 1’"]
    );
}

/// Mixer drag and drop: every strip dropped into every gap between the
/// strips (folders nested two deep, tracks at the top level between them)
/// shows exactly there afterwards — one undo step each, undone cleanly.
#[test]
fn every_strip_lands_where_it_is_dropped() {
    let mut s = Session::new(
        faderframe_project::Project::new("Order", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let add = |s: &mut Session, kind| s.add_track(kind).unwrap();
    let a = add(&mut s, TrackKind::Audio);
    let f1 = add(&mut s, TrackKind::Folder);
    let b = add(&mut s, TrackKind::Audio);
    let c = add(&mut s, TrackKind::Audio);
    let f2 = add(&mut s, TrackKind::Folder);
    let d = add(&mut s, TrackKind::Audio);
    let e = add(&mut s, TrackKind::Audio);
    let g = add(&mut s, TrackKind::Bus);
    let h = add(&mut s, TrackKind::Audio);
    for (t, f) in [(b, f1), (c, f1), (f2, f1), (d, f2), (e, f2)] {
        s.dispatch(Action::Edit(Command::SetTrackFolder {
            track: t,
            folder: Some(f),
        }))
        .unwrap();
    }
    // What the mixer shows: folder order without folders and the master.
    let strips = |s: &Session| -> Vec<TrackId> {
        s.project()
            .folder_order()
            .into_iter()
            .filter(|t| !matches!(t.kind, TrackKind::Master | TrackKind::Folder))
            .map(|t| t.id)
            .collect()
    };
    let start = strips(&s);
    assert_eq!(start, [a, b, c, d, e, g, h]);
    let before_all = s.project().clone();
    for &t in &start {
        let others: Vec<TrackId> = start.iter().copied().filter(|x| *x != t).collect();
        for gap in 0..=others.len() {
            let after = gap.checked_sub(1).map(|i| others[i]);
            let before = others.get(gap).copied();
            let mut want = others.clone();
            want.insert(gap, t);
            let steps = s.history_steps().0.len();
            s.dispatch(Action::PlaceTrack {
                track: t,
                after,
                before,
            })
            .unwrap();
            assert_eq!(strips(&s), want, "{t:?} into gap {gap}");
            // One step (none when it was there already), and back.
            let now = s.history_steps().0.len();
            assert!(now - steps <= 1);
            if now > steps {
                s.dispatch(Action::Undo).unwrap();
            }
            assert_eq!(strips(&s), start);
            assert_eq!(s.project().tracks, before_all.tracks);
        }
    }
}

/// Arranger drag and drop: rows include the folders (open), and a folder
/// moves with what it holds. Every track and folder dropped into every gap
/// between the other rows shows exactly there afterwards — under a
/// folder's header first in it — each one undo step.
#[test]
fn every_row_lands_where_it_is_dropped_folders_included() {
    let mut s = Session::new(
        faderframe_project::Project::new("Rows", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let add = |s: &mut Session, kind| s.add_track(kind).unwrap();
    let a = add(&mut s, TrackKind::Audio);
    let f1 = add(&mut s, TrackKind::Folder);
    let b = add(&mut s, TrackKind::Audio);
    let f2 = add(&mut s, TrackKind::Folder);
    let c = add(&mut s, TrackKind::Audio);
    let d = add(&mut s, TrackKind::Audio);
    let e = add(&mut s, TrackKind::Bus);
    for (t, f) in [(b, f1), (f2, f1), (c, f2)] {
        s.dispatch(Action::Edit(Command::SetTrackFolder {
            track: t,
            folder: Some(f),
        }))
        .unwrap();
    }
    let rows = |s: &Session| -> Vec<TrackId> {
        s.project()
            .folder_order()
            .into_iter()
            .filter(|t| t.kind != TrackKind::Master)
            .map(|t| t.id)
            .collect()
    };
    let start = rows(&s);
    assert_eq!(start, [a, f1, b, f2, c, d, e]);
    let before_all = s.project().tracks.clone();
    for &t in &start {
        // The block that moves: the track, and what it holds.
        let block: Vec<TrackId> = start
            .iter()
            .copied()
            .filter(|x| {
                *x == t
                    || s.project()
                        .track(*x)
                        .is_some_and(|xt| s.project().in_folder(xt, t))
            })
            .collect();
        let others: Vec<TrackId> = start
            .iter()
            .copied()
            .filter(|x| !block.contains(x))
            .collect();
        for gap in 0..=others.len() {
            let after = gap.checked_sub(1).map(|i| others[i]);
            let before = others.get(gap).copied();
            let mut want = others.clone();
            for (k, x) in block.iter().enumerate() {
                want.insert(gap + k, *x);
            }
            let steps = s.history_steps().0.len();
            s.dispatch(Action::PlaceTrack {
                track: t,
                after,
                before,
            })
            .unwrap();
            assert_eq!(
                rows(&s),
                want,
                "{t:?} into gap {gap} ({after:?} | {before:?})"
            );
            let now = s.history_steps().0.len();
            assert!(now - steps <= 1);
            if now > steps {
                s.dispatch(Action::Undo).unwrap();
            }
            assert_eq!(s.project().tracks, before_all);
        }
    }
    // A folder never goes into itself or a folder inside it.
    s.dispatch(Action::PlaceTrack {
        track: f1,
        after: Some(f2),
        before: Some(c),
    })
    .unwrap();
    assert_eq!(s.project().tracks, before_all);
}
