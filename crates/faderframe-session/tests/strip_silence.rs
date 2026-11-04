//! Strip Silence on synthetic hits: the dialog's preview finds them,
//! settings change it at once, and each mode is one undo step — the
//! silence taken out, the clip split, or the hits as numbered clips on a
//! track of their own with the original muted.
#![allow(clippy::unwrap_used)]

use faderframe_analysis::silence::SilenceSettings;
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::clip_envelope::{EnvelopeLane, EnvelopePoint};
use faderframe_project::{ClipContent, Project};
use faderframe_session::strip_silence::{StripMode, StripSilenceOp};
use faderframe_session::{Action, EditRange, SelectMode, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::time::Duration;

const SR: u32 = 48_000;

/// Hits of 100 ms at 0.5, 1.5 and 2.5 s in 3 s (stereo: the third only on
/// the right).
fn hits() -> [Vec<f32>; 2] {
    let mut l = vec![0.0f32; 3 * SR as usize];
    let mut r = l.clone();
    for (i, at) in [0.5, 1.5, 2.5].into_iter().enumerate() {
        let a = (at * f64::from(SR)) as usize;
        for k in 0..SR as usize / 10 {
            let v = 0.5 * (k as f32 * 0.05).sin() * (1.0 - k as f32 / 4_800.0);
            if i < 2 {
                l[a + k] = v;
            }
            r[a + k] = v;
        }
    }
    [l, r]
}

fn setup(name: &str) -> (Session, TrackId, ClipId, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("ff-strip-silence-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Hits.wav");
    write_wav(&file, &hits(), SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Hits", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    let track = s.project().clip(clip).unwrap().track;
    (s, track, clip, dir)
}

fn settings() -> SilenceSettings {
    SilenceSettings {
        threshold_db: -40.0,
        min_length: 0.02,
        pre_pad: 0.01,
        post_pad: 0.05,
        min_gap: 0.1,
    }
}

fn open(s: &mut Session, clip: ClipId) {
    s.dispatch(Action::StripSilence(StripSilenceOp::Settings(settings())))
        .unwrap();
    s.dispatch(Action::StripSilence(StripSilenceOp::Open {
        clips: vec![clip],
    }))
    .unwrap();
    assert!(s.wait_for_strip_silence(Duration::from_secs(20)));
}

/// Clip spans (start, end) in seconds, sorted.
fn spans(s: &Session, track: TrackId) -> Vec<(f64, f64, bool)> {
    let p = s.project();
    let rate = f64::from(SR);
    let mut v: Vec<(f64, f64, bool)> = p
        .clips_of(track)
        .into_iter()
        .map(|c| {
            let a = p.timeline.to_samples(c.start, rate) as f64 / rate;
            let len = match &c.content {
                ClipContent::Audio(a) => a.length as f64 / rate,
                _ => 0.0,
            };
            (a, a + len, c.muted)
        })
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.003
}

#[test]
fn the_preview_finds_the_hits_and_follows_the_settings() {
    let (mut s, _, clip, dir) = setup("preview");
    open(&mut s, clip);
    assert!(s.take_ui_requests().contains(&UiRequest::StripSilence));
    let p = s.strip_silence_preview().unwrap();
    let r = &p.regions[&clip];
    // Every channel counts: the third hit is on the right only.
    assert_eq!(r.len(), 3, "{r:?}");
    assert!(near(r[0].start as f64 / f64::from(SR), 0.49));
    assert!(near(r[2].end as f64 / f64::from(SR), 2.65));
    // A high threshold finds nothing.
    s.dispatch(Action::StripSilence(StripSilenceOp::Settings(
        SilenceSettings {
            threshold_db: -3.0,
            ..settings()
        },
    )))
    .unwrap();
    assert!(s.strip_silence_preview().unwrap().regions[&clip].is_empty());
    // A long minimum gap joins them all.
    s.dispatch(Action::StripSilence(StripSilenceOp::Settings(
        SilenceSettings {
            min_gap: 1.5,
            ..settings()
        },
    )))
    .unwrap();
    assert_eq!(s.strip_silence_preview().unwrap().regions[&clip].len(), 1);
    s.dispatch(Action::StripSilence(StripSilenceOp::Close))
        .unwrap();
    assert!(s.strip_silence_preview().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strip_takes_out_the_silence_in_one_step_and_keeps_the_envelope() {
    let (mut s, track, clip, dir) = setup("strip");
    // An envelope rides along into the pieces.
    s.dispatch(Action::SetClipEnvelope {
        clip,
        lane: EnvelopeLane::Volume,
        points: vec![
            EnvelopePoint::new(0, 0.0),
            EnvelopePoint::new(3 * SR as i64, -30.0),
        ],
    })
    .unwrap();
    open(&mut s, clip);
    let steps = s.history_steps().0.len();
    s.dispatch(Action::StripSilence(StripSilenceOp::Apply(
        StripMode::Strip,
    )))
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let v = spans(&s, track);
    assert_eq!(v.len(), 3, "{v:?}");
    assert!(near(v[0].0, 0.49) && near(v[0].1, 0.65), "{v:?}");
    assert!(near(v[1].0, 1.49) && near(v[2].1, 2.65), "{v:?}");
    // The second piece's envelope continues the original's curve.
    let second = s
        .project()
        .clips_of(track)
        .into_iter()
        .min_by_key(|c| (c.start.ticks() - MusicalTime::from_quarters(3.0).ticks()).abs())
        .map(|c| c.id)
        .unwrap();
    let e = s.clip_envelope(second).unwrap();
    let start = e.value_at(EnvelopeLane::Volume, 0.0).unwrap();
    assert!((start - (-30.0 * 1.49 / 3.0)).abs() < 0.1, "{start}");
    // The pieces are selected and the dialog looks at them.
    assert_eq!(s.selection.clips.len(), 3);
    assert_eq!(s.strip_silence_preview().unwrap().clips.len(), 3);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(spans(&s, track).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn separate_splits_at_the_edges_and_keeps_everything() {
    let (mut s, track, clip, dir) = setup("separate");
    open(&mut s, clip);
    s.dispatch(Action::StripSilence(StripSilenceOp::Apply(
        StripMode::Separate,
    )))
    .unwrap();
    let v = spans(&s, track);
    // Three hits and the four silences around them.
    assert_eq!(v.len(), 7, "{v:?}");
    assert!(near(v[0].0, 0.0) && near(v[6].1, 3.0));
    for w in v.windows(2) {
        assert!(near(w[0].1, w[1].0), "{v:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hits_go_to_new_numbered_clips_on_a_track_of_their_own() {
    let (mut s, track, clip, dir) = setup("new-clips");
    open(&mut s, clip);
    let tracks = s.project().tracks.len();
    s.dispatch(Action::StripSilence(StripSilenceOp::Apply(
        StripMode::NewClips,
    )))
    .unwrap();
    assert_eq!(s.project().tracks.len(), tracks + 1);
    // Right under the clip's track, which keeps its clip, muted.
    let i = s
        .project()
        .tracks
        .iter()
        .position(|t| t.id == track)
        .unwrap();
    let hits = s.project().tracks[i + 1].id;
    assert_eq!(spans(&s, track), vec![(0.0, 3.0, true)]);
    let mut names: Vec<String> = s
        .project()
        .clips_of(hits)
        .into_iter()
        .map(|c| c.name.clone())
        .collect();
    names.sort();
    assert_eq!(names, ["Hits 01", "Hits 02", "Hits 03"]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks);
    assert_eq!(spans(&s, track), vec![(0.0, 3.0, false)]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strip_silence_needs_audio_clips() {
    let (mut s, _, _, dir) = setup("needs");
    s.dispatch(Action::SelectClips {
        clips: Vec::new(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    assert!(
        s.dispatch(Action::StripSilence(StripSilenceOp::Open {
            clips: Vec::new()
        }))
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn on_an_edit_range_only_the_silence_inside_it_goes() {
    let (mut s, track, _, dir) = setup("range");
    s.dispatch(Action::SelectTracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })
    .unwrap();
    // 1 s to 3 s at 120 BPM.
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        MusicalTime::from_quarters(2.0),
        MusicalTime::from_quarters(6.0),
    ))))
    .unwrap();
    s.dispatch(Action::StripSilence(StripSilenceOp::Settings(settings())))
        .unwrap();
    s.dispatch(Action::StripSilence(StripSilenceOp::Open {
        clips: Vec::new(),
    }))
    .unwrap();
    assert!(s.wait_for_strip_silence(Duration::from_secs(20)));
    s.dispatch(Action::StripSilence(StripSilenceOp::Apply(
        StripMode::Strip,
    )))
    .unwrap();
    let v = spans(&s, track);
    // The first second (and its hit) stays whole.
    assert_eq!(v.len(), 3, "{v:?}");
    assert!(near(v[0].0, 0.0) && near(v[0].1, 1.0), "{v:?}");
    assert!(near(v[1].0, 1.49) && near(v[2].1, 2.65), "{v:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
