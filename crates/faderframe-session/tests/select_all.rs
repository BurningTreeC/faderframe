//! Select All in the arranger (issue #11), splits that keep every piece of
//! the selected clips selected (issue #10), and the selection staying the
//! target of editing commands while other controls are used.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Project, SourceSpec,
    StretchSettings, TrackKind,
};
use faderframe_session::{
    Action, EditFlag, EditMode, EditRange, EditTool, SelectMode, Session, TransportAction,
};
use faderframe_timeline::{GridDivision, MusicalTime};

const SR: u32 = 48_000;
const BAR: i64 = 2 * SR as i64;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

fn session(tracks: usize) -> (Session, Vec<TrackId>, AudioSourceId) {
    let mut s = Session::new(Project::new("Select", SR), None, EngineConfig::default()).unwrap();
    let ids = (0..tracks)
        .map(|_| s.add_track(TrackKind::Audio).unwrap())
        .collect();
    let source = AudioSourceId(9000);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "drums".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 16,
                    seed: 3,
                },
            },
        }),
    }))
    .unwrap();
    (s, ids, source)
}

fn add(s: &mut Session, t: TrackId, source: AudioSourceId, start: f64, bars: f64) -> ClipId {
    let id = ClipId(20_000 + s.project().clips.len() as u64);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "hit".into(),
            color: None,
            start: bar(start),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: (start * BAR as f64) as i64,
                length: (bars * BAR as f64) as i64,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
            }),
        }),
    }))
    .unwrap();
    id
}

fn locate(s: &mut Session, at: MusicalTime) {
    s.dispatch(Action::Transport(TransportAction::Locate(at)))
        .unwrap();
}

/// Starts (bars) of the selected clips on `t`.
fn selected_on(s: &Session, t: TrackId) -> Vec<f64> {
    s.project()
        .clips_of(t)
        .into_iter()
        .filter(|c| s.selection.clips.contains(&c.id))
        .map(|c| c.start.quarters() / 4.0)
        .collect()
}

#[test]
fn select_all_takes_every_clip_the_arranger_shows() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    // A launcher clip (in no track's list) and a closed folder.
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let first = s.project().track(bass).unwrap().clips[0];
    s.dispatch(Action::Launcher(
        faderframe_session::launcher::LauncherOp::SendClips(vec![first]),
    ))
    .unwrap();
    let launcher = s.project().launcher_clips();
    assert!(!launcher.is_empty());
    s.dispatch(Action::NewFolder { tracks: vec![bass] })
        .unwrap();
    let folder = s.project().track(bass).unwrap().folder.unwrap();
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    assert!(!s.track_shown(s.project().track(bass).unwrap()));
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(2.0),
    ))))
    .unwrap();
    s.dispatch(Action::SelectAllClips).unwrap();
    let p = s.project();
    let shown: Vec<ClipId> = s
        .arranger_rows()
        .into_iter()
        .flat_map(|t| p.track(t).unwrap().clips.clone())
        .collect();
    assert!(!shown.is_empty());
    assert_eq!(
        s.selection.clips.iter().copied().collect::<Vec<_>>(),
        {
            let mut v = shown.clone();
            v.sort();
            v
        },
        "every clip on a shown track"
    );
    assert!(s.selection.clips.iter().all(|c| !launcher.contains(c)));
    assert!(
        p.track(bass)
            .unwrap()
            .clips
            .iter()
            .all(|c| !s.selection.clips.contains(c)),
        "nothing hidden in a closed folder"
    );
    assert_eq!(s.selection.range, None, "the clips are what edits now take");
    // Their tracks are selected with them (as a rectangle selects).
    for c in &s.selection.clips {
        assert!(s.selection.tracks.contains(&p.clip(*c).unwrap().track));
    }
    // Opened again: Select All takes the folder's clips too.
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    s.dispatch(Action::SelectAllClips).unwrap();
    assert!(
        s.project()
            .track(bass)
            .unwrap()
            .clips
            .iter()
            .all(|c| s.selection.clips.contains(c))
    );
}

#[test]
fn splitting_keeps_every_piece_of_every_selected_clip_selected() {
    // Drums on three tracks, the same two bars; an unrelated clip on a
    // fourth track; a clip ending exactly where the first cut goes.
    let (mut s, t, src) = session(4);
    let kick = add(&mut s, t[0], src, 0.0, 2.0);
    let snare = add(&mut s, t[1], src, 0.0, 2.0);
    let hats = add(&mut s, t[2], src, 0.0, 2.0);
    let other = add(&mut s, t[3], src, 0.0, 2.0);
    let ends = add(&mut s, t[1], src, 3.0, 0.5);
    s.dispatch(Action::SelectClips {
        clips: vec![kick, snare, hats, ends],
        mode: SelectMode::Replace,
    })
    .unwrap();
    // Split at bar 0.5, then at bar 1 and 1.5 (the playhead moves on, the
    // selection carries on).
    for (n, at) in [0.5, 1.0, 1.5].into_iter().enumerate() {
        locate(&mut s, bar(at));
        s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
        for track in &t[..3] {
            assert_eq!(
                selected_on(&s, *track).iter().filter(|x| **x < 2.0).count(),
                n + 2,
                "every piece on every drum track stays selected"
            );
        }
        assert!(selected_on(&s, t[3]).is_empty(), "nothing else is selected");
        assert_eq!(s.history().undo_label(), Some("Split Clips"));
    }
    assert_eq!(selected_on(&s, t[0]), vec![0.0, 0.5, 1.0, 1.5]);
    assert!(s.selection.clips.contains(&ends), "the clip not cut stays");
    assert_eq!(s.project().clips_of(t[3]).len(), 1, "not selected, not cut");
    assert!(!s.selection.clips.contains(&other));
    // At a clip's end or start: nothing to cut, the selection stays.
    locate(&mut s, bar(3.5));
    let clips = s.project().clips.len();
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(s.project().clips.len(), clips);
    locate(&mut s, bar(3.0));
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(s.project().clips.len(), clips);
    assert!(s.selection.clips.contains(&ends));
    // Undo takes the last cut back (the pieces it made leave the
    // selection); redo cuts again.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(selected_on(&s, t[0]), vec![0.0, 0.5, 1.0]);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.project().clips_of(t[0]).len(), 4);
    // The audio is cut, not moved: the pieces play on from where the one
    // before stopped.
    let p = s.project();
    let kicks = p.clips_of(t[0]);
    for w in kicks.windows(2) {
        let (a, b) = (w[0].as_audio().unwrap(), w[1].as_audio().unwrap());
        assert_eq!(a.source_offset + a.length, b.source_offset);
    }
}

#[test]
fn zero_crossing_cuts_and_separate_keep_the_pieces_selected() {
    let (mut s, t, src) = session(2);
    let a = add(&mut s, t[0], src, 0.0, 4.0);
    let b = add(&mut s, t[1], src, 0.0, 4.0);
    s.dispatch(Action::SetEditFlag(EditFlag::SnapToZeroCrossings, true))
        .unwrap();
    s.dispatch(Action::SelectClips {
        clips: vec![a, b],
        mode: SelectMode::Replace,
    })
    .unwrap();
    locate(&mut s, bar(1.0) + MusicalTime::from_quarters(0.013));
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(s.selection.clips.len(), 4);
    // Separate at a range's edges: the selected clips' pieces stay
    // selected, and their tracks' unselected clips are cut but not
    // selected.
    s.dispatch(Action::SetEditFlag(EditFlag::SnapToZeroCrossings, false))
        .unwrap();
    s.dispatch(Action::SelectClips {
        clips: vec![a],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: t.clone(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(0.25),
        bar(0.5),
    ))))
    .unwrap();
    s.dispatch(Action::Separate).unwrap();
    assert_eq!(selected_on(&s, t[0]), vec![0.0, 0.25, 0.5]);
    assert!(selected_on(&s, t[1]).is_empty());
    assert_eq!(s.project().clips_of(t[1]).len(), 4);
}

#[test]
fn the_selection_stays_the_target_while_tools_modes_and_transport_change() {
    let (mut s, t, src) = session(2);
    let a = add(&mut s, t[0], src, 0.0, 1.0);
    let b = add(&mut s, t[1], src, 1.0, 1.0);
    s.dispatch(Action::SelectClips {
        clips: vec![a, b],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: t.clone(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    let selection = s.selection.clone();
    // What toolbar, tool selector and transport controls do.
    for action in [
        Action::SetEditTool(EditTool::Trim),
        Action::SetEditMode(EditMode::Slip),
        Action::SetGrid(GridDivision::Bar),
        Action::ToggleSnap,
        Action::SetEditFlag(EditFlag::FollowPlayhead, false),
        Action::Transport(TransportAction::ToggleLoop),
        Action::Transport(TransportAction::Locate(bar(6.0))),
        Action::SetEditTool(EditTool::Smart),
    ] {
        s.dispatch(action).unwrap();
        assert_eq!(s.selection.clips, selection.clips);
    }
    // The commands still act on the selected clips.
    s.dispatch(Action::Copy).unwrap();
    assert_eq!(s.clipboard().clip_count(), 2);
    s.dispatch(Action::Repeat(1)).unwrap();
    assert_eq!(s.project().clips_of(t[0]).len(), 2);
    assert_eq!(s.project().clips_of(t[1]).len(), 2);
    // With nothing selected the commands do nothing.
    s.dispatch(Action::SelectClips {
        clips: vec![],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let clips = s.project().clips.len();
    s.dispatch(Action::Repeat(1)).unwrap();
    s.dispatch(Action::Cut).unwrap();
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    s.dispatch(Action::Copy).unwrap();
    assert_eq!(s.project().clips.len(), clips);
    assert_eq!(
        s.clipboard().clip_count(),
        2,
        "Copy without a selection keeps the clipboard"
    );
}
