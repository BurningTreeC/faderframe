//! Live collaboration: sessions in one process, connected over this
//! machine's loopback, editing one project at once.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, Command, Project, SourceSpec, TrackKind,
};
use faderframe_session::collab::{CollabOp, CollabRole, Connection, project_hash};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-collab-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
        .id
}

fn edit(s: &mut Session, cmd: Command) {
    s.dispatch(Action::Edit(cmd)).unwrap();
}

/// A host on the loopback and a peer joined to it.
fn pair() -> (Session, Session) {
    let mut a = Session::demo(EngineConfig::default()).unwrap();
    a.dispatch(Action::Collab(CollabOp::Host {
        bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
        name: "Ana".into(),
    }))
    .unwrap();
    let invite = a.collaboration().unwrap().invite.unwrap();
    assert_eq!(invite.addresses.len(), 1);
    let mut b = Session::new(Project::new("Empty", 48_000), None, EngineConfig::default()).unwrap();
    b.dispatch(Action::Collab(CollabOp::Join {
        address: invite.addresses[0].clone(),
        code: invite.code.clone(),
        name: "Bo".into(),
    }))
    .unwrap();
    pump(&mut [&mut a, &mut b], |s| {
        s[1].collaboration()
            .is_some_and(|c| c.connection == Connection::Connected)
            && s[0].collab_settled()
            && s[1].collab_settled()
    });
    (a, b)
}

/// Tick every session until `done` (or fail after a while).
fn pump(sessions: &mut [&mut Session], done: impl Fn(&[&mut Session]) -> bool) {
    let t = Instant::now();
    loop {
        for s in sessions.iter_mut() {
            s.tick(0.005);
        }
        if done(sessions) {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "collaboration did not settle: {:#?}",
            sessions
                .iter()
                .map(|s| s.collaboration())
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Every session settled with the same project.
fn alike(sessions: &[&mut Session]) -> bool {
    sessions.iter().all(|s| s.collab_settled())
        && sessions
            .windows(2)
            .all(|w| project_hash(w[0].project()) == project_hash(w[1].project()))
}

fn notices(s: &mut Session) -> Vec<String> {
    s.notices().map(|n| n.text.clone()).collect()
}

#[test]
fn a_peer_joins_and_gets_the_hosts_project() {
    let (a, b) = pair();
    assert_eq!(b.project().name, a.project().name);
    assert_eq!(project_hash(a.project()), project_hash(b.project()));
    let status = b.collaboration().unwrap();
    assert_eq!(status.role, CollabRole::Peer);
    let names: Vec<&str> = status.people.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Ana", "Bo"]);
    assert!(status.people[1].me && status.people[0].host);
    // The host sees the peer.
    let host = a.collaboration().unwrap();
    assert_eq!(host.people.len(), 2);
    assert!(host.people[1].connected);
}

#[test]
fn edits_made_at_once_end_alike_everywhere() {
    let (mut a, mut b) = pair();
    let bass = track(&a, "Bass");
    let drums = track(&a, "Drums");
    // Both edit before hearing from the other.
    edit(
        &mut a,
        Command::SetTrackVolume {
            track: bass,
            db: -6.0,
        },
    );
    edit(
        &mut a,
        Command::RenameTrack {
            track: drums,
            name: "Kit A".into(),
        },
    );
    edit(
        &mut b,
        Command::SetTrackPan {
            track: bass,
            pan: 0.3,
        },
    );
    edit(
        &mut b,
        Command::RenameTrack {
            track: drums,
            name: "Kit B".into(),
        },
    );
    // New things on both sides: ids from their own ranges.
    a.dispatch(Action::AddTrack(TrackKind::Audio)).unwrap();
    b.dispatch(Action::AddTrack(TrackKind::Audio)).unwrap();
    b.dispatch(Action::AddMarker(MusicalTime::from_quarters(8.0)))
        .unwrap();
    // A gesture: many values, one thing.
    b.dispatch(Action::BeginGesture("Fader".into())).unwrap();
    for i in 0..20 {
        edit(
            &mut b,
            Command::SetTrackVolume {
                track: drums,
                db: -(i as f32) * 0.5,
            },
        );
    }
    b.dispatch(Action::EndGesture).unwrap();
    pump(&mut [&mut a, &mut b], alike);
    let p = a.project();
    let t = p.track(bass).unwrap();
    assert_eq!((t.volume_db, t.pan), (-6.0, 0.3));
    // The peer's rename came later: it wins.
    assert_eq!(p.track(drums).unwrap().name, "Kit B");
    assert_eq!(p.track(drums).unwrap().volume_db, -9.5);
    let audio = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count();
    let before = faderframe_project::demo::demo_project(48_000)
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count();
    assert_eq!(audio, before + 2, "both new tracks, with ids of their own");
    let ids: std::collections::BTreeSet<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), p.tracks.len());
    assert_eq!(p.markers.len(), b.project().markers.len());
    // Whoever had set it is told who changed it after.
    assert!(
        notices(&mut a)
            .iter()
            .any(|n| n.contains("Bo changed what you had just changed")),
        "the host is told"
    );
}

#[test]
fn each_undoes_their_own_steps() {
    let (mut a, mut b) = pair();
    let bass = track(&a, "Bass");
    let keys = track(&a, "Pad");
    let was = a.project().track(bass).unwrap().volume_db;
    edit(
        &mut b,
        Command::SetTrackVolume {
            track: bass,
            db: -11.5,
        },
    );
    pump(&mut [&mut a, &mut b], alike);
    edit(
        &mut a,
        Command::SetTrackPan {
            track: keys,
            pan: -0.5,
        },
    );
    pump(&mut [&mut a, &mut b], alike);
    // The peer's undo takes back its own step, not the host's later one.
    b.dispatch(Action::Undo).unwrap();

    pump(&mut [&mut a, &mut b], alike);
    for s in [&a, &b] {
        assert_eq!(s.project().track(bass).unwrap().volume_db, was);
        assert_eq!(s.project().track(keys).unwrap().pan, -0.5);
    }
    // And redo brings it back for everyone.
    b.dispatch(Action::Redo).unwrap();
    pump(&mut [&mut a, &mut b], alike);
    assert_eq!(a.project().track(bass).unwrap().volume_db, -11.5);
    // The host's undo takes back the host's step.
    a.dispatch(Action::Undo).unwrap();
    pump(&mut [&mut a, &mut b], alike);
    assert_eq!(b.project().track(keys).unwrap().pan, 0.0);
    assert_eq!(b.project().track(bass).unwrap().volume_db, -11.5);
}

#[test]
fn an_edit_of_something_removed_meanwhile_is_refused_and_told() {
    let (mut a, mut b) = pair();
    let drums = track(&a, "Drums");
    let clip = a.project().tracks[a
        .project()
        .tracks
        .iter()
        .position(|t| t.id == drums)
        .unwrap()]
    .clips[0];
    // The host removes the clip; the peer, not knowing yet, moves it.
    edit(&mut a, Command::RemoveClip { clip });
    edit(
        &mut b,
        Command::MoveClip {
            clip,
            track: drums,
            start: MusicalTime::from_quarters(16.0),
        },
    );
    pump(&mut [&mut a, &mut b], alike);
    assert!(a.project().clip(clip).is_none());
    assert!(b.project().clip(clip).is_none());
    let told = notices(&mut b);
    assert!(
        told.iter().any(|n| n.contains("could not")),
        "the peer is told: {told:?}"
    );
}

fn wav(dir: &Path, name: &str, seconds: f32, hz: f32) -> PathBuf {
    let rate = 48_000;
    let n = (seconds * rate as f32) as usize;
    let samples: Vec<f32> = (0..n)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5)
        .collect();
    let path = dir.join(name);
    write_wav(&path, &[samples], rate, WavFormat::Float32, false).unwrap();
    path
}

/// A source and a clip of a file on a new track (one step).
fn place_file(s: &mut Session, path: &Path, frames: i64) -> (TrackId, PathBuf) {
    s.dispatch(Action::AddTrack(TrackKind::Audio)).unwrap();
    let track = s
        .project()
        .tracks
        .iter()
        .rev()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap()
        .id;
    let mut ids = s.project().ids.clone();
    let source = ids.allocate();
    let clip = ids.allocate();
    let commands = vec![
        Command::AddSource {
            source: Box::new(AudioSource {
                id: source,
                name: "file".into(),
                spec: SourceSpec::File {
                    path: path.to_path_buf(),
                    channels: 1,
                    frames,
                    sample_rate: 48_000,
                },
            }),
        },
        Command::AddClip {
            clip: Box::new(Clip {
                id: clip,
                track,
                name: "file".into(),
                color: None,
                start: MusicalTime::ZERO,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source,
                    source_offset: 0,
                    length: frames,
                    gain_db: 0.0,
                    fades: Default::default(),
                    stretch: Default::default(),
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                    spectral: None,
                    envelope: None,
                }),
            }),
        },
    ];
    edit(
        s,
        Command::Batch {
            label: "Import".into(),
            commands,
        },
    );
    (track, path.to_path_buf())
}

fn source_path(s: &Session, track: TrackId) -> PathBuf {
    let p = s.project();
    let clip = p.track(track).unwrap().clips[0];
    let ClipContent::Audio(a) = &p.clip(clip).unwrap().content else {
        panic!()
    };
    match &p.sources[&a.source].spec {
        SourceSpec::File { path, .. } => path.clone(),
        SourceSpec::Generated { .. } => panic!(),
    }
}

#[test]
fn media_cross_by_their_content_both_ways() {
    let dir = tmp("media");
    let (mut a, mut b) = pair();
    // The host brings a file in: the peer fetches it.
    let host_file = wav(&dir, "Guitar.wav", 3.0, 220.0);
    let (on_host, _) = place_file(&mut a, &host_file, 3 * 48_000);
    pump(&mut [&mut a, &mut b], alike);
    let here = source_path(&b, on_host);
    assert_ne!(here, host_file, "the peer keeps it in its own media folder");
    assert_eq!(
        std::fs::read(&here).unwrap(),
        std::fs::read(&host_file).unwrap()
    );
    // The peer brings one in: the host fetches it from the peer.
    let peer_file = wav(&dir, "Voice.wav", 2.0, 330.0);
    let (on_peer, _) = place_file(&mut b, &peer_file, 2 * 48_000);
    pump(&mut [&mut a, &mut b], alike);
    let at_host = source_path(&a, on_peer);
    assert_ne!(at_host, peer_file);
    assert_eq!(
        std::fs::read(&at_host).unwrap(),
        std::fs::read(&peer_file).unwrap()
    );
    // Both sessions play them (nothing offline).
    let status = b.collaboration().unwrap();
    assert_eq!(status.media.waiting, 0);
    assert!(status.media.failed.is_empty());
}

#[test]
fn a_third_session_gets_a_file_the_host_fetches_from_another_peer() {
    let dir = tmp("three");
    let (mut a, mut b) = pair();
    let invite = a.collaboration().unwrap().invite.unwrap();
    let mut c = Session::new(Project::new("C", 48_000), None, EngineConfig::default()).unwrap();
    c.dispatch(Action::Collab(CollabOp::Join {
        address: invite.addresses[0].clone(),
        code: invite.code,
        name: "Cy".into(),
    }))
    .unwrap();
    pump(&mut [&mut a, &mut b, &mut c], alike);
    let file = wav(&dir, "Bass DI.wav", 1.0, 55.0);
    let (track, _) = place_file(&mut b, &file, 48_000);
    pump(&mut [&mut a, &mut b, &mut c], alike);
    let at_c = source_path(&c, track);
    assert_eq!(std::fs::read(at_c).unwrap(), std::fs::read(&file).unwrap());
    let people = a.collaboration().unwrap().people;
    assert_eq!(people.len(), 3);
}

#[test]
fn a_peer_that_loses_the_connection_comes_back_with_what_it_did() {
    let (mut a, mut b) = pair();
    let bass = track(&a, "Bass");
    let keys = track(&a, "Pad");
    b.collab_break_connection();
    assert!(matches!(
        b.collaboration().unwrap().connection,
        Connection::Lost { .. }
    ));
    // Both go on working meanwhile.
    edit(
        &mut b,
        Command::SetTrackVolume {
            track: bass,
            db: -9.0,
        },
    );
    edit(
        &mut a,
        Command::SetTrackPan {
            track: keys,
            pan: 0.7,
        },
    );
    a.dispatch(Action::AddTrack(TrackKind::Bus)).unwrap();
    assert_eq!(b.collaboration().unwrap().waiting, 1);
    // It is back by itself: what it missed arrives, what it did is sent.
    pump(&mut [&mut a, &mut b], alike);
    assert_eq!(b.collaboration().unwrap().connection, Connection::Connected);
    for s in [&a, &b] {
        assert_eq!(s.project().track(bass).unwrap().volume_db, -9.0);
        assert_eq!(s.project().track(keys).unwrap().pan, 0.7);
    }
    let host = a.collaboration().unwrap();
    assert_eq!(host.people.len(), 2, "the same peer, not a new one");
    // Chat reaches everyone, in the host's order.
    b.dispatch(Action::Collab(CollabOp::Chat("back again".into())))
        .unwrap();
    pump(&mut [&mut a, &mut b], |s| {
        s.iter().all(|s| {
            s.collaboration()
                .is_some_and(|c| c.chat.iter().any(|l| l.text == "back again"))
        })
    });
}

#[test]
fn the_host_leaving_ends_the_session_and_the_peer_keeps_its_copy() {
    let (mut a, mut b) = pair();
    let bass = track(&a, "Bass");
    edit(
        &mut a,
        Command::SetTrackVolume {
            track: bass,
            db: -4.0,
        },
    );
    pump(&mut [&mut a, &mut b], alike);
    a.dispatch(Action::Collab(CollabOp::Leave)).unwrap();
    pump(&mut [&mut a, &mut b], |s| !s[1].collaborating());
    assert!(matches!(
        b.collaboration().unwrap().connection,
        Connection::Ended { .. }
    ));
    assert_eq!(b.project().track(bass).unwrap().volume_db, -4.0);
    assert!(b.is_dirty(), "the copy is unsaved");
    // Edits stay local now.
    edit(
        &mut b,
        Command::SetTrackVolume {
            track: bass,
            db: -1.0,
        },
    );
    assert_eq!(a.project().track(bass).unwrap().volume_db, -4.0);
}

#[test]
fn a_wrong_code_does_not_get_in() {
    let mut a = Session::demo(EngineConfig::default()).unwrap();
    a.dispatch(Action::Collab(CollabOp::Host {
        bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
        name: "Ana".into(),
    }))
    .unwrap();
    let invite = a.collaboration().unwrap().invite.unwrap();
    let mut b = Session::new(Project::new("Mine", 48_000), None, EngineConfig::default()).unwrap();
    let wrong = faderframe_collab::InviteCode::new().unwrap().to_string();
    b.dispatch(Action::Collab(CollabOp::Join {
        address: invite.addresses[0].clone(),
        code: wrong,
        name: "Eve".into(),
    }))
    .unwrap();
    pump(&mut [&mut a, &mut b], |s| !s[1].collaborating());
    assert_eq!(b.project().name, "Mine", "its own project stays");
    let Connection::Ended { reason } = b.collaboration().unwrap().connection else {
        panic!()
    };
    assert!(reason.contains("invite code"), "{reason}");
}

#[test]
fn a_peer_listens_along_to_the_hosts_mix() {
    if !faderframe_codec::opus_available() {
        return;
    }
    let (mut a, mut b) = pair();
    // The host plays on a (silent) device: its master goes out as Opus.
    a.start_audio(
        vec![Box::new(faderframe_audio::dummy::DummyBackend::default())],
        &faderframe_session::AudioPreferences {
            sample_rate: Some(48_000),
            buffer_size: Some(256),
            ..Default::default()
        },
    )
    .unwrap();
    a.dispatch(Action::Transport(faderframe_session::TransportAction::Play))
        .unwrap();
    b.dispatch(Action::Collab(CollabOp::ListenAlong(true)))
        .unwrap();
    pump(&mut [&mut a, &mut b], |s| {
        s[0].collaboration()
            .is_some_and(|c| c.people.iter().any(|p| p.listening))
            && s[1].listen_along_status().is_some_and(|l| l.received >= 25)
    });
    let l = b.listen_along_status().unwrap();
    assert!(l.on && l.buffered > 0.1, "{l:?}");
    assert_eq!(l.lost, 0);
    b.dispatch(Action::Collab(CollabOp::ListenAlong(false)))
        .unwrap();
    pump(&mut [&mut a, &mut b], |s| {
        s[0].collaboration()
            .is_some_and(|c| c.people.iter().all(|p| !p.listening))
    });
    a.stop_audio();
}

#[test]
fn the_whole_project_again_keeps_what_waits() {
    let (mut a, mut b) = pair();
    let bass = track(&a, "Bass");
    let drums = track(&a, "Drums");
    edit(
        &mut a,
        Command::SetTrackVolume {
            track: bass,
            db: -7.5,
        },
    );
    // The peer edits and asks for the whole project before it heard of
    // the host's edit.
    edit(
        &mut b,
        Command::SetTrackPan {
            track: drums,
            pan: -0.25,
        },
    );
    b.dispatch(Action::Collab(CollabOp::Resync)).unwrap();
    pump(&mut [&mut a, &mut b], alike);
    for s in [&a, &b] {
        assert_eq!(s.project().track(bass).unwrap().volume_db, -7.5);
        assert_eq!(s.project().track(drums).unwrap().pan, -0.25);
    }
}
