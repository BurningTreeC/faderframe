#![allow(clippy::unwrap_used)]

use faderframe_core::{ClipId, NoteId, SendId, TrackId};
use faderframe_project::demo::demo_project;
use faderframe_project::{
    AuxSend, ClipContent, Command, EditError, History, Impact, InputRouting, MidiNote, MonitorMode,
    OutputRouting, Project, SendTap, Track, TrackColor, TrackKind, file,
};
use faderframe_timeline::MusicalTime;
use faderframe_workspace::WorkspaceSet;

/// Projects equal apart from the ID allocator (allocations are deliberately
/// not rolled back by undo so IDs stay unique).
fn assert_same(a: &Project, b: &Project) {
    let mut a = a.clone();
    a.ids = b.ids.clone();
    assert_eq!(&a, b);
}

fn track_by_name(p: &Project, name: &str) -> TrackId {
    p.tracks.iter().find(|t| t.name == name).unwrap().id
}

fn add_bus(p: &mut Project, h: &mut History, name: &str) -> TrackId {
    let id: TrackId = p.ids.allocate();
    let t = Track::new(id, TrackKind::Bus, name, TrackColor::palette(3));
    h.apply(
        p,
        Command::AddTrack {
            track: Box::new(t),
            index: 0,
        },
    )
    .unwrap();
    id
}

#[test]
fn demo_project_is_consistent() {
    let mut p = demo_project(48_000);
    assert!(p.repair().is_empty(), "demo should need no repairs");
    assert_eq!(
        p.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Master)
            .count(),
        1
    );
    assert!(p.tracks.len() >= 8);
    assert!(p.clips.len() >= 5);
    for c in p.clips.values() {
        assert!(p.track(c.track).unwrap().clips.contains(&c.id));
    }
    // Every routing target exists and the routing is acyclic.
    for (a, b) in p.routing_edges() {
        assert!(p.track(a).is_some() && p.track(b).is_some());
        assert!(!p.reaches(b, a, None), "cycle between {a} and {b}");
    }
}

#[test]
fn fader_gesture_is_one_undo_step() {
    let mut p = demo_project(48_000);
    let mut h = History::default();
    let bass = track_by_name(&p, "Bass");
    let original = p.track(bass).unwrap().volume_db;
    h.begin("Fader drag");
    for i in 0..50 {
        h.apply(
            &mut p,
            Command::SetTrackVolume {
                track: bass,
                db: -20.0 + i as f32 * 0.3,
            },
        )
        .unwrap();
    }
    h.end();
    assert!((p.track(bass).unwrap().volume_db - (-20.0 + 49.0 * 0.3)).abs() < 1e-4);
    let undone = h.undo(&mut p).unwrap().unwrap();
    assert_eq!(undone.impact, Impact::Params);
    assert_eq!(p.track(bass).unwrap().volume_db, original);
    assert!(!h.can_undo());
    h.redo(&mut p).unwrap();
    assert!((p.track(bass).unwrap().volume_db - (-20.0 + 49.0 * 0.3)).abs() < 1e-4);
}

#[test]
fn multi_step_undo_redo_preserves_order() {
    let mut p = demo_project(48_000);
    let mut h = History::default();
    let bass = track_by_name(&p, "Bass");
    h.begin("Rename and recolour");
    h.apply(
        &mut p,
        Command::RenameTrack {
            track: bass,
            name: "Sub".into(),
        },
    )
    .unwrap();
    h.apply(
        &mut p,
        Command::RenameTrack {
            track: bass,
            name: "Sub Bass".into(),
        },
    )
    .unwrap();
    h.apply(
        &mut p,
        Command::SetTrackColor {
            track: bass,
            color: TrackColor::rgb(1, 2, 3),
        },
    )
    .unwrap();
    h.end();
    h.undo(&mut p).unwrap();
    assert_eq!(p.track(bass).unwrap().name, "Bass");
    h.redo(&mut p).unwrap();
    assert_eq!(p.track(bass).unwrap().name, "Sub Bass");
    assert_eq!(p.track(bass).unwrap().color, TrackColor::rgb(1, 2, 3));
    h.undo(&mut p).unwrap();
    h.redo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    assert_eq!(p.track(bass).unwrap().name, "Bass");
    assert_eq!(h.redo_label(), Some("Rename and recolour"));
}

#[test]
fn routing_cycles_and_invalid_targets_are_rejected() {
    let mut p = demo_project(48_000);
    let mut h = History::default();
    let drums = track_by_name(&p, "Drums");
    let drum_bus = track_by_name(&p, "Drum Bus");
    let echo = track_by_name(&p, "Echo");
    let master = p.master_id().unwrap();

    // Drum Bus → Drums would close a loop (Drums → Drum Bus exists) and
    // Drums is not a summing track anyway.
    let err = h
        .apply(
            &mut p,
            Command::SetTrackOutput {
                track: drum_bus,
                output: OutputRouting::Track { track: drums },
            },
        )
        .unwrap_err();
    assert!(matches!(err, EditError::InvalidRouting(_)));

    // Echo → Drum Bus is fine; Drum Bus → Echo afterwards is a loop.
    let bus2 = add_bus(&mut p, &mut h, "Bus 2");
    h.apply(
        &mut p,
        Command::SetTrackOutput {
            track: echo,
            output: OutputRouting::Track { track: bus2 },
        },
    )
    .unwrap();
    let send = AuxSend {
        id: p.ids.allocate(),
        target: echo,
        level_db: -6.0,
        tap: SendTap::PreFader,
        enabled: true,
    };
    let err = h
        .apply(
            &mut p,
            Command::AddSend {
                track: bus2,
                send,
                index: None,
            },
        )
        .unwrap_err();
    assert_eq!(err, EditError::FeedbackLoop);

    // The master can only feed hardware.
    let err = h
        .apply(
            &mut p,
            Command::SetTrackOutput {
                track: master,
                output: OutputRouting::Track { track: bus2 },
            },
        )
        .unwrap_err();
    assert!(matches!(err, EditError::InvalidRouting(_)));
    assert_eq!(
        h.apply(&mut p, Command::RemoveTrack { track: master })
            .unwrap_err(),
        EditError::CannotRemoveMaster
    );
}

#[test]
fn removing_a_bus_reroutes_and_undo_restores_everything() {
    let mut p = demo_project(48_000);
    let before = p.clone();
    let mut h = History::default();
    let drums = track_by_name(&p, "Drums");
    let drum_bus = track_by_name(&p, "Drum Bus");
    let echo = track_by_name(&p, "Echo");
    let impact = h
        .apply(&mut p, Command::RemoveTrack { track: drum_bus })
        .unwrap();
    assert_eq!(impact, Impact::Graph);
    assert_eq!(p.track(drums).unwrap().output, OutputRouting::Master);
    assert!(p.track(drum_bus).is_none());

    // Removing the echo aux removes the sends that targeted it.
    h.apply(&mut p, Command::RemoveTrack { track: echo })
        .unwrap();
    assert!(
        p.tracks
            .iter()
            .all(|t| t.sends.iter().all(|s| s.target != echo))
    );

    h.undo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    assert_same(&p, &before);
}

#[test]
fn removing_a_track_with_clips_restores_clips() {
    let mut p = demo_project(48_000);
    let before = p.clone();
    let mut h = History::default();
    let pad = track_by_name(&p, "Pad");
    let n_clips = p.clips.len();
    h.apply(&mut p, Command::RemoveTrack { track: pad })
        .unwrap();
    assert!(p.clips.len() < n_clips);
    h.undo(&mut p).unwrap();
    assert_same(&p, &before);
}

#[test]
fn clips_move_between_compatible_tracks_only() {
    let mut p = demo_project(48_000);
    let mut h = History::default();
    let drums = track_by_name(&p, "Drums");
    let bass = track_by_name(&p, "Bass");
    let synth = track_by_name(&p, "Lead Synth");
    let clip = p.track(drums).unwrap().clips[0];
    h.apply(
        &mut p,
        Command::MoveClip {
            clip,
            track: bass,
            start: MusicalTime::from_quarters_i(8),
        },
    )
    .unwrap();
    assert!(p.track(bass).unwrap().clips.contains(&clip));
    assert!(!p.track(drums).unwrap().clips.contains(&clip));
    let err = h
        .apply(
            &mut p,
            Command::MoveClip {
                clip,
                track: synth,
                start: MusicalTime::ZERO,
            },
        )
        .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)));
    h.undo(&mut p).unwrap();
    assert!(p.track(drums).unwrap().clips.contains(&clip));
    assert_eq!(p.clip(clip).unwrap().track, drums);
}

#[test]
fn split_audio_and_midi_clips() {
    let mut p = demo_project(48_000);
    let before = p.clone();
    let mut h = History::default();
    let drums = track_by_name(&p, "Drums");
    let clip = p.track(drums).unwrap().clips[0];
    let original = p.clip(clip).unwrap().clone();
    let at = p.timeline.meter.bar_start(3);
    let new_clip: ClipId = p.ids.allocate();
    h.apply(&mut p, Command::SplitClip { clip, at, new_clip })
        .unwrap();
    let left = p.clip(clip).unwrap().as_audio().unwrap().clone();
    let right = p.clip(new_clip).unwrap().as_audio().unwrap().clone();
    let orig = original.as_audio().unwrap();
    assert_eq!(left.length + right.length, orig.length);
    assert_eq!(right.source_offset, orig.source_offset + left.length);
    assert_eq!(p.clip(new_clip).unwrap().start, at);

    let synth = track_by_name(&p, "Lead Synth");
    let midi = p.track(synth).unwrap().clips[0];
    let notes_before = p.clip(midi).unwrap().as_midi().unwrap().notes.len();
    let mid = p.clip(midi).unwrap().start + MusicalTime::from_quarters_i(4);
    let midi_right: ClipId = p.ids.allocate();
    h.apply(
        &mut p,
        Command::SplitClip {
            clip: midi,
            at: mid,
            new_clip: midi_right,
        },
    )
    .unwrap();
    let l = p.clip(midi).unwrap().as_midi().unwrap();
    let r = p.clip(midi_right).unwrap().as_midi().unwrap();
    assert_eq!(l.notes.len() + r.notes.len(), notes_before);
    assert!(l.notes.iter().all(|n| n.end() <= l.length));

    h.undo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    assert_same(&p, &before);
}

#[test]
fn note_editing_round_trip() {
    let mut p = demo_project(48_000);
    let before = p.clone();
    let mut h = History::default();
    let synth = track_by_name(&p, "Lead Synth");
    let clip = p.track(synth).unwrap().clips[0];
    let id: NoteId = p.ids.allocate();
    let note = MidiNote {
        id,
        start: MusicalTime::ZERO,
        length: MusicalTime::QUARTER,
        key: 200, // sanitised to 127
        velocity: 0,
        channel: 0,
        muted: false,
    };
    h.apply(&mut p, Command::AddNote { clip, note }).unwrap();
    let stored = *p.clip(clip).unwrap().as_midi().unwrap().note(id).unwrap();
    assert_eq!((stored.key, stored.velocity), (127, 1));
    h.begin("Drag note");
    for k in 60..70 {
        h.apply(
            &mut p,
            Command::UpdateNote {
                clip,
                note: MidiNote { key: k, ..stored },
            },
        )
        .unwrap();
    }
    h.end();
    h.apply(&mut p, Command::RemoveNote { clip, note: id })
        .unwrap();
    for _ in 0..3 {
        h.undo(&mut p).unwrap();
    }
    assert_same(&p, &before);
}

#[test]
fn failed_batch_rolls_back() {
    let mut p = demo_project(48_000);
    let before = p.clone();
    let bass = track_by_name(&p, "Bass");
    let res = Command::Batch {
        label: "bad".into(),
        commands: vec![
            Command::SetTrackVolume {
                track: bass,
                db: -30.0,
            },
            Command::RemoveSend {
                track: bass,
                send: SendId(999_999),
            },
        ],
    }
    .apply(&mut p);
    assert!(res.is_err());
    assert_same(&p, &before);
}

#[test]
fn project_file_round_trip_with_workspace() {
    let p = demo_project(44_100);
    let mut ws = WorkspaceSet::default();
    ws.switch_to(2);
    let text = file::to_string(&p, Some(&ws)).unwrap();
    let loaded = file::from_str(&text).unwrap();
    assert!(loaded.notes.is_empty(), "{:?}", loaded.notes);
    assert_eq!(loaded.project, p);
    assert_eq!(loaded.workspace, Some(ws));

    let dir = std::env::temp_dir().join(format!("faderframe-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("demo.ffproj");
    file::save(&path, &p, None).unwrap();
    let again = file::load(&path).unwrap();
    assert_eq!(again.project, p);
    assert!(again.workspace.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_format_errors_and_migration() {
    assert!(matches!(
        file::from_str("{}"),
        Err(file::FileError::WrongFormat)
    ));
    assert!(matches!(
        file::from_str("not json"),
        Err(file::FileError::Json(_))
    ));
    let too_new = r#"{"format":"faderframe-project","version":999,"project":{}}"#;
    assert!(matches!(
        file::from_str(too_new),
        Err(file::FileError::TooNew { found: 999, .. })
    ));

    // A version-0 draft with "channels" instead of "tracks" upgrades cleanly.
    let p = Project::new("old", 48_000);
    let mut value: serde_json::Value =
        serde_json::from_str(&file::to_string(&p, None).unwrap()).unwrap();
    value["version"] = 0.into();
    let tracks = value["project"]
        .as_object_mut()
        .unwrap()
        .remove("tracks")
        .unwrap();
    value["project"]["channels"] = tracks;
    let loaded = file::from_str(&value.to_string()).unwrap();
    assert_eq!(loaded.project.tracks.len(), 1);
    assert!(loaded.notes.iter().any(|n| n.contains("version 0")));

    // Version 1: instrument (and MIDI) tracks had no MIDI input; they get
    // one.
    let demo = demo_project(48_000);
    let mut value: serde_json::Value =
        serde_json::from_str(&file::to_string(&demo, None).unwrap()).unwrap();
    value["version"] = 1.into();
    for t in value["project"]["tracks"].as_array_mut().unwrap() {
        t["input"] = serde_json::json!({ "type": "none" });
        t["monitor"] = "off".into();
    }
    let loaded = file::from_str(&value.to_string()).unwrap();
    for t in &loaded.project.tracks {
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            assert_eq!(t.input, InputRouting::all_midi());
            assert_eq!(t.monitor, MonitorMode::Auto);
        } else {
            assert_eq!(t.input, InputRouting::None, "{}", t.name);
            assert_eq!(t.monitor, MonitorMode::Off);
        }
    }
}

#[test]
fn clip_content_replacement_is_undoable() {
    let mut p = demo_project(48_000);
    let mut h = History::default();
    let bass = track_by_name(&p, "Bass");
    let clip = p.track(bass).unwrap().clips[0];
    let c = p.clip(clip).unwrap().clone();
    let ClipContent::Audio(mut a) = c.content.clone() else {
        panic!()
    };
    a.gain_db = -6.0;
    a.length /= 2;
    h.apply(
        &mut p,
        Command::SetClipContent {
            clip,
            start: c.start,
            content: Box::new(ClipContent::Audio(a)),
        },
    )
    .unwrap();
    assert_eq!(p.clip(clip).unwrap().as_audio().unwrap().gain_db, -6.0);
    h.undo(&mut p).unwrap();
    assert_eq!(p.clip(clip).unwrap(), &c);
}

#[test]
fn sources_are_undoable_and_protected_while_in_use() {
    use faderframe_project::{AudioSource, Command, EditError, History, SourceSpec};
    let mut p = faderframe_project::Project::new("S", 48_000);
    let track: faderframe_core::TrackId = p.ids.allocate();
    p.tracks.insert(
        0,
        faderframe_project::Track::new(
            track,
            faderframe_project::TrackKind::Audio,
            "A",
            faderframe_project::TrackColor::palette(0),
        ),
    );
    let id = p.ids.allocate();
    let source = AudioSource {
        id,
        name: "take".into(),
        spec: SourceSpec::File {
            path: "/media/take.wav".into(),
            channels: 1,
            frames: 48_000,
            sample_rate: 48_000,
        },
    };
    let clip_id = p.ids.allocate();
    let clip = faderframe_project::Clip {
        id: clip_id,
        track,
        name: "take".into(),
        color: None,
        start: faderframe_timeline::MusicalTime::ZERO,
        muted: false,
        content: faderframe_project::ClipContent::Audio(faderframe_project::AudioClip {
            source: id,
            source_offset: 0,
            length: 48_000,
            gain_db: 0.0,
            fades: Default::default(),
            stretch: Default::default(),
            reversed: false,
            warp: None,
        }),
    };
    let mut h = History::default();
    // A clip cannot reference an unknown source.
    assert!(matches!(
        h.apply(
            &mut p,
            Command::AddClip {
                clip: Box::new(clip.clone())
            }
        ),
        Err(EditError::UnknownSource(_))
    ));
    h.apply(
        &mut p,
        Command::Batch {
            label: "Import".into(),
            commands: vec![
                Command::AddSource {
                    source: Box::new(source),
                },
                Command::AddClip {
                    clip: Box::new(clip),
                },
            ],
        },
    )
    .unwrap();
    assert!(matches!(
        h.apply(&mut p, Command::RemoveSource { source: id }),
        Err(EditError::SourceInUse(_))
    ));
    h.undo(&mut p).unwrap();
    assert!(p.sources.is_empty() && p.clips.is_empty());
    h.redo(&mut p).unwrap();
    assert_eq!(p.sources.len(), 1);
    assert_eq!(p.clips.len(), 1);
}

#[test]
fn time_signature_changes_undo() {
    use faderframe_timeline::TimeSignature;
    let mut p = demo_project(48_000);
    let before = p.clone();
    let mut h = History::default();
    let seven_eight = TimeSignature::new(7, 8).unwrap();
    h.apply(
        &mut p,
        Command::SetTimeSignature {
            bar: 4,
            signature: Some(seven_eight),
        },
    )
    .unwrap();
    assert_eq!(p.timeline.meter.signature_of_bar(5), seven_eight);
    assert_eq!(
        p.timeline.meter.signature_of_bar(3),
        TimeSignature::FOUR_FOUR
    );
    assert_eq!(
        Command::SetTimeSignature {
            bar: 4,
            signature: None
        }
        .impact(),
        Impact::Timeline
    );
    // The first meter cannot be removed.
    assert!(matches!(
        h.apply(
            &mut p,
            Command::SetTimeSignature {
                bar: 0,
                signature: None
            }
        ),
        Err(EditError::Invalid(_))
    ));
    h.undo(&mut p).unwrap();
    assert_same(&p, &before);
}

#[test]
fn crosstalk_is_saved_undoable_and_defaults_off_in_older_files() {
    let mut p = Project::new("Crosstalk", 48_000);
    assert!(!p.crosstalk);
    let mut h = History::default();
    assert_eq!(
        h.apply(&mut p, Command::SetCrosstalk { enabled: true })
            .unwrap(),
        Impact::Graph
    );
    let text = file::to_string(&p, None).unwrap();
    assert!(file::from_str(&text).unwrap().project.crosstalk);
    h.undo(&mut p).unwrap();
    assert!(!p.crosstalk);
    h.redo(&mut p).unwrap();
    assert!(p.crosstalk);
    let mut old: serde_json::Value = serde_json::from_str(&text).unwrap();
    old["project"].as_object_mut().unwrap().remove("crosstalk");
    assert!(!file::from_str(&old.to_string()).unwrap().project.crosstalk);
}
