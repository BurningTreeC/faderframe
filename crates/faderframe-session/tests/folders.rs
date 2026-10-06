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

fn order(s: &Session) -> Vec<String> {
    s.project()
        .folder_order()
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[test]
fn a_folder_holds_the_selected_tracks_and_keeps_them_when_removed() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
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
    let mut s = Session::demo(EngineConfig::default()).unwrap();
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
    let mut s = Session::demo(EngineConfig::default()).unwrap();
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
