//! Snap to Samples (positions off the grid land on whole samples) and
//! Snap to Zero Crossings (cuts and trims of audio clips move to the
//! nearest zero crossing).
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::{Action, ClipEdge, EditFlag, EditRange, SelectMode, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

/// Two seconds of a 100 Hz sine (zero crossings every 240 samples) on an
/// audio track at the start, the track selected.
fn session() -> (Session, TrackId, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-session-snapping-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Sine.wav");
    let sine: Vec<f32> = (0..2 * SR as usize)
        .map(|i| (0.5 * (std::f64::consts::TAU * 100.0 * i as f64 / f64::from(SR)).sin()) as f32)
        .collect();
    write_wav(&file, &[sine], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Snap", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    let track = s.project().clip(clip).unwrap().track;
    s.dispatch(Action::SelectTracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })
    .unwrap();
    (s, track, clip)
}

fn at_sample(s: &Session, frame: i64) -> MusicalTime {
    s.project().timeline.to_musical(frame, f64::from(SR))
}

fn sample_of(s: &Session, t: MusicalTime) -> i64 {
    s.project().timeline.to_samples(t, f64::from(SR))
}

/// The clips' source offsets, by start.
fn offsets(s: &Session, track: TrackId) -> Vec<i64> {
    let mut clips: Vec<_> = s
        .project()
        .clips_of(track)
        .into_iter()
        .map(|c| (c.start, c.as_audio().unwrap().source_offset))
        .collect();
    clips.sort();
    clips.into_iter().map(|(_, o)| o).collect()
}

#[test]
fn cuts_move_to_zero_crossings_when_asked() {
    let (mut s, track, _) = session();
    let range = EditRange::new(at_sample(&s, 1_000), at_sample(&s, 30_100));
    // Off: the cuts where the range is.
    s.dispatch(Action::SetEditRange(Some(range))).unwrap();
    s.dispatch(Action::Separate).unwrap();
    assert_eq!(offsets(&s, track), [0, 1_000, 30_100]);
    s.dispatch(Action::Undo).unwrap();
    // On: at the nearest crossings (960 and 30 000).
    s.dispatch(Action::SetEditFlag(EditFlag::SnapToZeroCrossings, true))
        .unwrap();
    s.dispatch(Action::SetEditRange(Some(range))).unwrap();
    s.dispatch(Action::Separate).unwrap();
    let o = offsets(&s, track);
    assert_eq!(o.len(), 3);
    assert!((o[1] - 960).abs() <= 1, "{o:?}");
    assert!((o[2] - 30_000).abs() <= 1, "{o:?}");
    s.dispatch(Action::Undo).unwrap();
    // A trim of the start too.
    let clip = s.project().clips_of(track)[0].id;
    s.dispatch(Action::TrimClips {
        clips: vec![clip],
        edge: ClipEdge::Start,
        by: at_sample(&s, 2_500).ticks(),
        stretch: false,
    })
    .unwrap();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert!((a.source_offset - 2_400).abs() <= 1, "{}", a.source_offset);
}

#[test]
fn positions_round_to_whole_samples_only_when_asked() {
    let (mut s, _, _) = session();
    // An odd number of ticks: not a whole sample; rounded, it is.
    let odd = MusicalTime(12_345);
    let on_sample = |s: &Session, t: MusicalTime| at_sample(s, sample_of(s, t)) == t;
    assert!(!on_sample(&s, odd));
    let r = s.on_sample(odd);
    assert!(on_sample(&s, r));
    assert!((r.ticks() - odd.ticks()).abs() < 100);
    s.dispatch(Action::SetEditFlag(EditFlag::SnapToSamples, false))
        .unwrap();
    assert_eq!(s.on_sample(odd), odd);
}

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

/// Two MIDI tracks — a clip over quarters 2–6 on the first, one over
/// 6.2–10 on the second — a marker at 12, a section 14–18, the loop
/// 20–22 and the edit selection 24–26; the playhead at the start.
fn objects() -> (Session, ClipId, ClipId) {
    use faderframe_project::TrackKind;
    let mut s = Session::new(Project::new("Objects", SR), None, EngineConfig::default()).unwrap();
    let a = s.add_track(TrackKind::Midi).unwrap();
    let b = s.add_track(TrackKind::Midi).unwrap();
    let clip = |s: &mut Session, track: TrackId, from: f64, to: f64| {
        s.dispatch(Action::CreateMidiClip {
            track,
            start: q(from),
            length: q(to - from),
        })
        .unwrap();
        s.project().clips_of(track)[0].id
    };
    let ca = clip(&mut s, a, 2.0, 6.0);
    let cb = clip(&mut s, b, 6.2, 10.0);
    s.dispatch(Action::AddMarker(q(12.0))).unwrap();
    s.dispatch(Action::AddSection {
        start: q(14.0),
        end: q(18.0),
    })
    .unwrap();
    s.dispatch(Action::Transport(
        faderframe_session::TransportAction::SetLoop(faderframe_project::MusicalRange::new(
            q(20.0),
            q(22.0),
        )),
    ))
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(q(24.0), q(26.0)))))
        .unwrap();
    (s, ca, cb)
}

/// Snap to Objects (issue #7): clip edges on this and other tracks,
/// markers, sections, the loop, the selection and the playhead attract
/// within reach; the nearest wins; what is being edited never does.
#[test]
fn objects_within_reach_attract_the_nearest_and_never_what_is_edited() {
    use faderframe_session::{SnapObject, SnapSkip};
    let (mut s, ca, cb) = objects();
    assert!(s.editor.snap_objects, "on by default");
    let reach = q(0.1);
    let none = SnapSkip::default();
    let near = |s: &Session, t: f64, skip: &SnapSkip| s.snap_to_object(q(t), reach, skip);
    // Clip edges on either track: the nearest one.
    let hit = near(&s, 6.05, &none).unwrap();
    assert_eq!((hit.at, hit.object), (q(6.0), SnapObject::ClipEnd(ca)));
    let hit = near(&s, 6.12, &none).unwrap();
    assert_eq!((hit.at, hit.object), (q(6.2), SnapObject::ClipStart(cb)));
    assert_eq!(hit.track, s.project().clip(cb).map(|c| c.track));
    // Out of reach: nothing (the grid decides then).
    assert_eq!(near(&s, 6.4, &none), None);
    assert_eq!(s.snap_to_object(q(6.05), q(0.01), &none), None);
    // The clips being moved or trimmed are skipped.
    assert_eq!(near(&s, 6.05, &SnapSkip::clips(&[ca])), None);
    // Markers, sections, loop, selection, playhead.
    let kind = |t: f64, skip: &SnapSkip| near(&s, t, skip).map(|h| (h.at, h.object));
    let marker = s.project().markers[0].id;
    let section = s.project().sections[0].id;
    assert_eq!(
        kind(12.03, &none),
        Some((q(12.0), SnapObject::Marker(marker)))
    );
    let skip_marker = SnapSkip {
        marker: Some(marker),
        ..SnapSkip::default()
    };
    assert_eq!(kind(12.03, &skip_marker), None, "the dragged marker");
    assert_eq!(
        kind(17.95, &none),
        Some((q(18.0), SnapObject::SectionEnd(section)))
    );
    assert_eq!(kind(20.0, &none), Some((q(20.0), SnapObject::LoopStart)));
    assert_eq!(kind(20.0, &SnapSkip::loop_bounds()), None);
    assert_eq!(
        kind(25.98, &none),
        Some((q(26.0), SnapObject::SelectionEnd))
    );
    assert_eq!(kind(25.98, &SnapSkip::selection_bounds()), None);
    // The playhead (stopped), wherever it was put.
    s.dispatch(Action::Transport(
        faderframe_session::TransportAction::Locate(q(30.0)),
    ))
    .unwrap();
    let hit = near(&s, 30.02, &none).unwrap();
    assert_eq!(hit.object, SnapObject::Playhead);
    assert!((hit.at.quarters() - 30.0).abs() < 1e-3, "{:?}", hit.at);
    // Off: nothing attracts.
    s.dispatch(Action::SetEditFlag(EditFlag::SnapToObjects, false))
        .unwrap();
    assert_eq!(near(&s, 6.05, &none), None);
}

/// A moving group snaps by whichever of its edges is nearest an object:
/// the shift lands that edge exactly on it.
#[test]
fn a_group_snaps_by_whichever_edge_is_nearest_an_object() {
    use faderframe_session::{SnapObject, SnapSkip};
    let (s, ca, _) = objects();
    let reach = q(0.1);
    // Leading edge 0.03 from clip A's end, trailing 0.04 from the marker.
    let (shift, hit) = s
        .snap_edges_to_object(&[q(5.97), q(11.96)], reach, &SnapSkip::default())
        .unwrap();
    assert_eq!(
        (shift, hit.object),
        (q(6.0) - q(5.97), SnapObject::ClipEnd(ca))
    );
    // Now the trailing edge is nearer.
    let (shift, hit) = s
        .snap_edges_to_object(&[q(5.9), q(11.98)], reach, &SnapSkip::default())
        .unwrap();
    assert_eq!(hit.at, q(12.0));
    assert_eq!(q(11.98) + shift, q(12.0));
    assert_eq!(
        s.snap_edges_to_object(&[q(5.5), q(11.5)], reach, &SnapSkip::default()),
        None
    );
}

/// Clips in a closed folder are not shown, so nothing snaps to them.
#[test]
fn clips_in_a_closed_folder_are_not_targets() {
    use faderframe_session::SnapSkip;
    let (mut s, ca, _) = objects();
    let track = s.project().clip(ca).unwrap().track;
    s.dispatch(Action::NewFolder {
        tracks: vec![track],
    })
    .unwrap();
    let folder = s.project().track(track).unwrap().folder.unwrap();
    let skip = SnapSkip::default();
    assert!(s.snap_to_object(q(6.05), q(0.1), &skip).is_some());
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    assert!(!s.folder_open(folder));
    assert_eq!(s.snap_to_object(q(6.05), q(0.1), &skip), None);
}
