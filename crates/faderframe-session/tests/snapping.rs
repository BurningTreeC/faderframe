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
