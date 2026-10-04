//! Markers, arrangement sections and tempo changes.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Marker, Project};
use faderframe_session::lanes::GlobalLane;
use faderframe_session::{Action, Session};
use faderframe_timeline::{MusicalTime, TempoCurve};

fn session() -> Session {
    Session::new(Project::new("Lanes", 48_000), None, EngineConfig::default()).unwrap()
}

fn q(x: f64) -> MusicalTime {
    MusicalTime::from_quarters(x)
}

#[test]
fn markers_are_added_moved_renamed_and_undone() {
    let mut s = session();
    s.dispatch(Action::AddMarker(q(8.0))).unwrap();
    s.dispatch(Action::AddMarker(q(4.0))).unwrap();
    let names: Vec<_> = s.project().markers.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, ["Marker 2", "Marker 1"], "sorted by position");
    let m = s.project().markers[1].clone();
    s.dispatch(Action::Edit(Command::UpdateMarker {
        marker: Marker {
            position: q(2.0),
            name: "Verse".into(),
            ..m.clone()
        },
    }))
    .unwrap();
    assert_eq!(s.project().markers[0].name, "Verse");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().markers[1], m);
}

#[test]
fn sections_get_song_names_and_need_a_length() {
    let mut s = session();
    for (a, b) in [(0.0, 16.0), (16.0, 48.0), (48.0, 80.0), (80.0, 112.0)] {
        s.dispatch(Action::AddSection {
            start: q(a),
            end: q(b),
        })
        .unwrap();
    }
    let names: Vec<_> = s
        .project()
        .sections
        .iter()
        .map(|x| x.name.as_str())
        .collect();
    assert_eq!(names, ["Intro", "Verse", "Chorus", "Verse"]);
    let (v1, v2) = (&s.project().sections[1], &s.project().sections[3]);
    assert_eq!(v1.color, v2.color, "same name, same colour");
    assert!(
        s.dispatch(Action::AddSection {
            start: q(4.0),
            end: q(4.0),
        })
        .is_err()
    );
    s.dispatch(Action::ShowGlobalLane(GlobalLane::Arranger, false))
        .unwrap();
    assert!(!s.editor.lanes.arranger);
}

#[test]
fn tempo_points_are_added_dragged_ramped_and_removed() {
    let mut s = session();
    s.dispatch(Action::AddTempoPoint(q(16.0))).unwrap();
    let pts = s.project().timeline.tempo.points().to_vec();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[1].bpm, 120.0, "keeps the tempo there");
    // Drag: one gesture, one undo step.
    s.dispatch(Action::BeginGesture("Tempo".into())).unwrap();
    for bpm in [125.0, 130.0, 140.0] {
        s.dispatch(Action::SetTempoPoint {
            index: 1,
            position: q(20.0),
            bpm,
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    let p = s.project().timeline.tempo.points()[1];
    assert_eq!((p.position, p.bpm), (q(20.0), 140.0));
    // At 140 BPM after beat 20, beat 24 is (20 × 0.5 s) + (4 × 60/140 s) in.
    let secs = s.project().timeline.tempo.musical_to_seconds(q(24.0));
    assert!((secs - (10.0 + 4.0 * 60.0 / 140.0)).abs() < 1e-9, "{secs}");
    // The first point stays at the start.
    s.dispatch(Action::SetTempoPoint {
        index: 0,
        position: q(3.0),
        bpm: 100.0,
    })
    .unwrap();
    assert_eq!(
        s.project().timeline.tempo.points()[0].position,
        MusicalTime::ZERO
    );
    s.dispatch(Action::SetTempoRamp {
        index: 0,
        ramp: true,
    })
    .unwrap();
    assert_eq!(
        s.project().timeline.tempo.points()[0].curve,
        TempoCurve::Linear
    );
    s.dispatch(Action::RemoveTempoPoint(1)).unwrap();
    assert_eq!(s.project().timeline.tempo.points().len(), 1);
    for _ in 0..3 {
        s.dispatch(Action::Undo).unwrap();
    }
    let p = s.project().timeline.tempo.points()[1];
    assert_eq!(p.bpm, 140.0, "undo restores the dragged point");
}

/// Intro, Verse, Chorus (a bar each, a MIDI note per bar) on a synth track.
fn song() -> Session {
    use faderframe_project::{Clip, ClipContent, MidiClip, MidiNote, Track, TrackColor, TrackKind};
    let mut p = Project::new("Song", 48_000);
    let t = p.ids.allocate();
    p.tracks.push(Track::new(
        t,
        TrackKind::Instrument,
        "Keys",
        TrackColor::PALETTE[0],
    ));
    for i in 0..3 {
        let id = p.ids.allocate();
        let note = MidiNote {
            id: p.ids.allocate(),
            start: MusicalTime::ZERO,
            length: q(1.0),
            key: 60 + i as u8,
            velocity: 100,
            channel: 0,
            muted: false,
        };
        p.clips.insert(
            id,
            Clip {
                id,
                track: t,
                name: String::new(),
                color: None,
                start: q(4.0 * i as f64),
                muted: false,
                content: ClipContent::Midi(MidiClip {
                    length: q(4.0),
                    notes: vec![note],
                    ..Default::default()
                }),
            },
        );
        p.tracks.last_mut().unwrap().clips.push(id);
    }
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    for i in 0..3 {
        s.dispatch(Action::AddSection {
            start: q(4.0 * i as f64),
            end: q(4.0 * (i + 1) as f64),
        })
        .unwrap();
    }
    s
}

/// Section names and the first note of the clip in each bar.
fn layout(s: &Session) -> (String, Vec<u8>) {
    use faderframe_project::ClipContent;
    let p = s.project();
    let names = p.sections.iter().map(|x| &x.name[..1]).collect::<String>();
    // As the arranger sees them (through the track's clip list).
    let track = p.tracks.last().unwrap().id;
    let clips = p.clips_of(track);
    assert_eq!(
        clips.len(),
        p.clips.len(),
        "every clip is listed on its track"
    );
    let keys = clips
        .iter()
        .filter_map(|c| match &c.content {
            ClipContent::Midi(m) => m.notes.first().map(|n| n.key),
            _ => None,
        })
        .collect();
    (names, keys)
}

#[test]
fn sections_move_copy_and_delete_with_their_content() {
    let mut s = song();
    assert_eq!(layout(&s), ("IVC".into(), vec![60, 61, 62]));
    let chorus = s.project().sections[2].id;
    let intro = s.project().sections[0].id;
    // Chorus first (one undo step).
    s.dispatch(Action::MoveSection {
        section: chorus,
        to: MusicalTime::ZERO,
        copy: false,
    })
    .unwrap();
    assert_eq!(layout(&s), ("CIV".into(), vec![62, 60, 61]));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(layout(&s), ("IVC".into(), vec![60, 61, 62]));
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(layout(&s), ("CIV".into(), vec![62, 60, 61]));
    // Intro one later (swap with the verse).
    s.dispatch(Action::SwapSection {
        section: intro,
        later: true,
    })
    .unwrap();
    assert_eq!(layout(&s), ("CVI".into(), vec![62, 61, 60]));
    // A copy of the chorus at the end, then the original chorus duplicated.
    s.dispatch(Action::MoveSection {
        section: chorus,
        to: q(12.0),
        copy: true,
    })
    .unwrap();
    assert_eq!(layout(&s), ("CVIC".into(), vec![62, 61, 60, 62]));
    s.dispatch(Action::DuplicateSection(chorus)).unwrap();
    assert_eq!(layout(&s), ("CCVIC".into(), vec![62, 62, 61, 60, 62]));
    // Delete the verse with its content: the rest moves up.
    let verse = s.project().sections[2].id;
    s.dispatch(Action::DeleteSectionContent(verse)).unwrap();
    assert_eq!(layout(&s), ("CCIC".into(), vec![62, 62, 60, 62]));
    for _ in 0..5 {
        s.dispatch(Action::Undo).unwrap();
    }
    assert_eq!(layout(&s), ("IVC".into(), vec![60, 61, 62]));
}

#[test]
fn the_master_panel_is_a_workspace_setting() {
    use faderframe_session::WorkspaceAction;
    let mut s = session();
    let mastering = s
        .workspace()
        .workspaces
        .iter()
        .position(|w| w.name == "Mastering")
        .unwrap();
    assert!(!s.master_panel(), "off where the project starts");
    s.dispatch(Action::Workspace(WorkspaceAction::Switch(mastering)))
        .unwrap();
    assert!(s.master_panel(), "on in Mastering");
    s.dispatch(Action::Workspace(WorkspaceAction::ToggleMasterPanel))
        .unwrap();
    assert!(!s.master_panel());
    s.dispatch(Action::Workspace(WorkspaceAction::Switch(0)))
        .unwrap();
    s.dispatch(Action::Workspace(WorkspaceAction::ToggleMasterPanel))
        .unwrap();
    assert!(s.master_panel(), "each workspace has its own");
}
