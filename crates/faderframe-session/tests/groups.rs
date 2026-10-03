//! Multi-track edits of selected tracks, track groups and VCAs.
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_core::gain::SILENCE_DB;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, OutputRouting, TrackKind};
use faderframe_session::{Action, SelectMode, Session};

fn demo() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn id(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn volume(s: &Session, t: TrackId) -> f32 {
    s.project().track(t).unwrap().volume_db
}

fn set_volume(s: &mut Session, track: TrackId, db: f32) {
    s.dispatch(Action::Edit(Command::SetTrackVolume { track, db }))
        .unwrap();
}

fn select(s: &mut Session, tracks: &[TrackId]) {
    s.dispatch(Action::SelectTracks {
        tracks: tracks.to_vec(),
        mode: SelectMode::Replace,
    })
    .unwrap();
}

#[test]
fn selected_tracks_follow_relatively_and_keep_their_balance_in_a_gesture() {
    let mut s = demo();
    let (pluck, pad, bass) = (id(&s, "Pluck"), id(&s, "Pad"), id(&s, "Bass"));
    set_volume(&mut s, pluck, -6.0);
    set_volume(&mut s, pad, -10.0);
    let bass_before = volume(&s, bass);
    select(&mut s, &[pluck, pad]);

    s.dispatch(Action::BeginGesture("Fader".into())).unwrap();
    set_volume(&mut s, pluck, -12.0);
    assert_eq!(volume(&s, pad), -16.0);
    // Down to -inf and back: the offset survives within the gesture.
    set_volume(&mut s, pluck, SILENCE_DB);
    assert_eq!(volume(&s, pad), SILENCE_DB);
    set_volume(&mut s, pluck, -3.0);
    assert_eq!(volume(&s, pad), -7.0);
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(volume(&s, bass), bass_before, "unselected tracks stay");

    // One undo step for all of them.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!((volume(&s, pluck), volume(&s, pad)), (-6.0, -10.0));

    // Switches are set alike; an unselected track only changes itself.
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: pad,
        on: true,
    }))
    .unwrap();
    assert!(s.project().track(pluck).unwrap().mute);
    set_volume(&mut s, bass, -20.0);
    assert_eq!(volume(&s, pluck), -6.0);

    // Pan moves relatively (clamped), sends to the same return too.
    let pan = |s: &Session, t: TrackId| s.project().track(t).unwrap().pan;
    let (pluck_pan, pad_pan) = (pan(&s, pluck), pan(&s, pad));
    s.dispatch(Action::Edit(Command::SetTrackPan {
        track: pluck,
        pan: pluck_pan + 0.25,
    }))
    .unwrap();
    assert_eq!(pan(&s, pad), (pad_pan + 0.25).clamp(-1.0, 1.0));
    s.dispatch(Action::Edit(Command::SetTrackPan {
        track: pluck,
        pan: 1.0,
    }))
    .unwrap();
    assert_eq!(
        pan(&s, pad),
        1.0f32.min(pad_pan + 1.0 - pluck_pan).max(-1.0)
    );
    let send = |s: &Session, t: TrackId| s.project().track(t).unwrap().sends[0].clone();
    let (a, b) = (send(&s, pluck), send(&s, pad));
    s.dispatch(Action::Edit(Command::SetSendLevel {
        track: pluck,
        send: a.id,
        db: a.level_db + 3.0,
    }))
    .unwrap();
    assert_eq!(send(&s, pad).level_db, b.level_db + 3.0);

    // Routing: both go to the bus.
    let bus = id(&s, "Drum Bus");
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: pad,
        output: OutputRouting::Track { track: bus },
    }))
    .unwrap();
    assert_eq!(
        s.project().track(pluck).unwrap().output,
        OutputRouting::Track { track: bus }
    );
}

#[test]
fn groups_link_their_members_while_active() {
    let mut s = demo();
    let (pluck, pad, bass) = (id(&s, "Pluck"), id(&s, "Pad"), id(&s, "Bass"));
    set_volume(&mut s, pluck, -6.0);
    set_volume(&mut s, pad, -10.0);
    select(&mut s, &[pluck, pad]);
    s.dispatch(Action::GroupSelectedTracks).unwrap();
    let group = s.project().groups[0].id;
    assert_eq!(s.project().group_members(group), [pluck, pad]);

    // Selecting one member selects the group.
    select(&mut s, &[bass]);
    select(&mut s, &[pluck]);
    assert!(s.selection.tracks.contains(&pad));
    select(&mut s, &[bass]);

    set_volume(&mut s, pluck, -8.0);
    assert_eq!(volume(&s, pad), -12.0);
    s.dispatch(Action::Edit(Command::SetTrackSolo {
        track: pad,
        on: true,
    }))
    .unwrap();
    assert!(s.project().track(pluck).unwrap().solo);

    // Unlinking volume, then deactivating the group.
    let mut link = s.project().group(group).unwrap().link;
    link.volume = false;
    s.dispatch(Action::SetGroupLink { group, link }).unwrap();
    set_volume(&mut s, pluck, -2.0);
    assert_eq!(volume(&s, pad), -12.0);
    s.dispatch(Action::SetGroupActive {
        group,
        active: false,
    })
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackSolo {
        track: pad,
        on: false,
    }))
    .unwrap();
    assert!(s.project().track(pluck).unwrap().solo);

    s.dispatch(Action::RenameGroup {
        group,
        name: "Synths".into(),
    })
    .unwrap();
    assert_eq!(s.project().group(group).unwrap().name, "Synths");
    s.dispatch(Action::DeleteGroup(group)).unwrap();
    assert!(s.project().groups.is_empty());
    assert!(s.project().track(pluck).unwrap().group.is_none());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().group_members(group), [pluck, pad]);
}

#[test]
fn vcas_take_members_and_refuse_loops() {
    let mut s = demo();
    let (pluck, pad) = (id(&s, "Pluck"), id(&s, "Pad"));
    s.dispatch(Action::AddTrack(TrackKind::Vca)).unwrap();
    let vca = id(&s, "VCA 1");
    s.dispatch(Action::AddTrack(TrackKind::Vca)).unwrap();
    let outer = id(&s, "VCA 2");
    select(&mut s, &[pluck, pad]);
    s.dispatch(Action::AssignSelectedToVca(vca)).unwrap();
    assert_eq!(s.project().vca_members(vca), [pluck, pad]);
    s.dispatch(Action::Edit(Command::SetTrackVca {
        track: vca,
        vca: Some(outer),
    }))
    .unwrap();
    let chain: Vec<TrackId> = s
        .project()
        .vca_chain(s.project().track(pluck).unwrap())
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(chain, [vca, outer]);
    // The outer VCA cannot follow the inner one.
    assert!(
        s.dispatch(Action::Edit(Command::SetTrackVca {
            track: outer,
            vca: Some(vca),
        }))
        .is_err()
    );
    // A muted VCA mutes its members (and the members of nested VCAs).
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: outer,
        on: true,
    }))
    .unwrap();
    let p = s.project();
    assert!(p.effectively_muted(p.track(pad).unwrap(), None));
    let menu: Vec<String> = s.group_menu(outer).into_iter().map(|e| e.label).collect();
    assert!(
        menu.iter().any(|l| l == "Assign Selected Tracks"),
        "{menu:?}"
    );
}
