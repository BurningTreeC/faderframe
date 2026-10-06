//! Pro-style editing: range operations, edit modes, trims, nudge, Tab.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Project, SourceSpec,
    StretchSettings, TrackKind, Warp, WarpMarker,
};
use faderframe_session::{
    Action, ClipEdge, EditFlag, EditMode, EditRange, NudgeTarget, NudgeValue, SelectMode, Session,
    TransportAction,
};
use faderframe_timeline::{GridDivision, MusicalTime};

const SR: u32 = 48_000;

fn bar(n: f64) -> MusicalTime {
    // 120 BPM, 4/4.
    MusicalTime::from_quarters(4.0 * n)
}

/// A session with one audio track and a 16-bar source.
fn setup() -> (Session, TrackId, AudioSourceId) {
    let mut s = Session::new(Project::new("Edit", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    let source: AudioSourceId = AudioSourceId(9000);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "loop".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 16,
                    seed: 1,
                },
            },
        }),
    }))
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })
    .unwrap();
    (s, t, source)
}

fn add_clip(s: &mut Session, t: TrackId, source: AudioSourceId, start: f64, bars: f64) -> ClipId {
    let id = ClipId(10_000 + s.project().clips.len() as u64);
    let frames = (bars * 4.0 * 0.5 * SR as f64) as i64;
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "c".into(),
            color: None,
            start: bar(start),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: (start * 4.0 * 0.5 * SR as f64) as i64,
                length: frames,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
            }),
        }),
    }))
    .unwrap();
    id
}

/// (start, end) in bars of the track's clips, sorted.
fn layout(s: &Session, t: TrackId) -> Vec<(f64, f64)> {
    let p = s.project();
    let mut v: Vec<(f64, f64)> = p
        .clips_of(t)
        .iter()
        .map(|c| {
            (
                c.start.quarters() / 4.0,
                c.end(&p.timeline, p.sample_rate).quarters() / 4.0,
            )
        })
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v.iter()
        .map(|&(a, b)| ((a * 1000.0).round() / 1000.0, (b * 1000.0).round() / 1000.0))
        .collect()
}

fn range(s: &mut Session, a: f64, b: f64) {
    s.dispatch(Action::SetEditRange(Some(EditRange::new(bar(a), bar(b)))))
        .unwrap();
}

#[test]
fn clearing_a_range_splits_and_shuffle_closes_the_gap() {
    let (mut s, t, src) = setup();
    add_clip(&mut s, t, src, 0.0, 8.0);
    range(&mut s, 2.0, 3.0);
    s.dispatch(Action::ClearRange).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (3.0, 8.0)]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 8.0)]);
    // Delete with a range clears it too; Shuffle closes the gap.
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    s.dispatch(Action::DeleteSelection).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (2.0, 7.0)]);
    // The audio after the cut is the same audio as before (bar 3 onwards).
    let p = s.project();
    let right = p
        .clips_of(t)
        .into_iter()
        .find(|c| c.start == bar(2.0))
        .unwrap();
    let ClipContent::Audio(a) = &right.content else {
        panic!()
    };
    assert_eq!(a.source_offset, 3 * 4 * SR as i64 / 2);
}

#[test]
fn separate_trim_and_insert_silence_work_on_the_range() {
    let (mut s, t, src) = setup();
    add_clip(&mut s, t, src, 0.0, 8.0);
    range(&mut s, 2.0, 3.0);
    s.dispatch(Action::Separate).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (2.0, 3.0), (3.0, 8.0)]);
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::TrimToSelection).unwrap();
    assert_eq!(layout(&s, t), vec![(2.0, 3.0)]);
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::InsertSilence).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (3.0, 9.0)]);
    assert_eq!(s.history().undo_label(), Some("Insert Silence"));
}

#[test]
fn copy_paste_duplicate_and_repeat() {
    let (mut s, t, src) = setup();
    add_clip(&mut s, t, src, 0.0, 2.0);
    range(&mut s, 0.0, 1.0);
    s.dispatch(Action::RepeatRange(1)).unwrap();
    // The first bar now also plays at bar 1 (over the original second bar).
    assert_eq!(layout(&s, t), vec![(0.0, 1.0), (1.0, 2.0)]);
    let p = s.project();
    let copy = p
        .clips_of(t)
        .into_iter()
        .find(|c| c.start == bar(1.0))
        .unwrap();
    let ClipContent::Audio(a) = &copy.content else {
        panic!()
    };
    assert_eq!(a.source_offset, 0, "a copy of bar 0");
    // The selection moved onto the copy, so repeating continues.
    assert_eq!(s.selection.range, Some(EditRange::new(bar(1.0), bar(2.0))));
    s.dispatch(Action::RepeatRange(2)).unwrap();
    assert_eq!(layout(&s, t).last(), Some(&(3.0, 4.0)));
    // Copy, move the playhead, paste.
    range(&mut s, 0.0, 1.0);
    s.dispatch(Action::CopyRange).unwrap();
    s.dispatch(Action::Transport(TransportAction::Locate(bar(6.0))))
        .unwrap();
    s.dispatch(Action::PasteRange).unwrap();
    assert_eq!(layout(&s, t).last(), Some(&(6.0, 7.0)));
}

#[test]
fn nudging_and_trimming_clips() {
    let (mut s, t, src) = setup();
    let c = add_clip(&mut s, t, src, 1.0, 2.0);
    s.dispatch(Action::SelectClips {
        clips: vec![c],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetNudge(NudgeValue::Grid(GridDivision::Note(4))))
        .unwrap();
    s.dispatch(Action::Nudge {
        forward: true,
        target: NudgeTarget::Move,
    })
    .unwrap();
    assert_eq!(layout(&s, t), vec![(1.25, 3.25)]);
    s.dispatch(Action::Nudge {
        forward: false,
        target: NudgeTarget::TrimEnd,
    })
    .unwrap();
    assert_eq!(layout(&s, t), vec![(1.25, 3.0)]);
    // Trimming the start earlier reveals audio before it…
    s.dispatch(Action::TrimClip {
        clip: c,
        edge: ClipEdge::Start,
        to: bar(1.0),
    })
    .unwrap();
    assert_eq!(layout(&s, t), vec![(1.0, 3.0)]);
    // …but not before the source starts.
    s.dispatch(Action::TrimClip {
        clip: c,
        edge: ClipEdge::Start,
        to: MusicalTime::ZERO,
    })
    .unwrap();
    // (The nudge moved the clip a quarter bar later than its audio, so the
    // source's first frame is at bar 0.25.)
    let start = layout(&s, t)[0].0;
    assert!(
        (start - 0.25).abs() < 1e-3,
        "clamped at the source start: {start}"
    );
}

#[test]
fn shuffle_moves_keep_clips_butted() {
    let (mut s, t, src) = setup();
    let a = add_clip(&mut s, t, src, 0.0, 1.0);
    let _b = add_clip(&mut s, t, src, 1.0, 2.0);
    let _c = add_clip(&mut s, t, src, 3.0, 1.0);
    // Drop the first clip near the end of the last one.
    s.dispatch(Action::ShuffleClip {
        clip: a,
        track: t,
        at: bar(3.9),
    })
    .unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (2.0, 3.0), (3.0, 4.0)]);
    let moved = s.project().clip(a).unwrap().start;
    assert_eq!(moved, bar(3.0), "the moved clip is last");
}

#[test]
fn tab_walks_clip_boundaries_and_link_moves_the_playhead() {
    let (mut s, t, src) = setup();
    add_clip(&mut s, t, src, 1.0, 1.0);
    add_clip(&mut s, t, src, 3.0, 1.0);
    let at = |s: &Session| s.playhead().quarters() / 4.0;
    for expect in [1.0, 2.0, 3.0, 4.0] {
        s.dispatch(Action::TabTo {
            forward: true,
            extend: false,
        })
        .unwrap();
        assert_eq!(at(&s), expect);
    }
    s.dispatch(Action::TabTo {
        forward: false,
        extend: true,
    })
    .unwrap();
    assert_eq!(s.selection.range, Some(EditRange::new(bar(3.0), bar(4.0))));
    // A range moves the playhead to its start while linked …
    range(&mut s, 2.0, 2.5);
    assert_eq!(at(&s), 2.0);
    // … and not when unlinked.
    s.dispatch(Action::SetEditFlag(EditFlag::LinkTimeline, false))
        .unwrap();
    range(&mut s, 5.0, 6.0);
    assert_eq!(at(&s), 2.0);
}

#[test]
fn warped_clips_split_with_their_time_map() {
    let (mut s, t, src) = setup();
    let c = add_clip(&mut s, t, src, 0.0, 2.0);
    // Squeeze the second bar's first half: source bar 1.5 at output bar 1.
    let bar_frames = 4 * SR as i64 / 2;
    let mut clip = s.project().clip(c).unwrap().clone();
    if let ClipContent::Audio(a) = &mut clip.content {
        a.warp = Some(Warp {
            source_length: a.length,
            markers: vec![WarpMarker {
                at: bar_frames,
                source: bar_frames * 3 / 2,
            }],
            algorithm: Default::default(),
        });
    }
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip: c,
        start: clip.start,
        content: Box::new(clip.content.clone()),
    }))
    .unwrap();
    range(&mut s, 1.0, 1.0);
    s.dispatch(Action::SelectClips {
        clips: vec![c],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Separate).unwrap();
    let p = s.project();
    let right = p
        .clips_of(t)
        .into_iter()
        .find(|x| x.start == bar(1.0))
        .unwrap();
    let ClipContent::Audio(a) = &right.content else {
        panic!()
    };
    assert_eq!(
        a.source_offset,
        bar_frames * 3 / 2,
        "continues where the warp left off"
    );
    assert_eq!(a.source_at(a.length as f64), (2 * bar_frames) as f64);
    let _ = TransportAction::Play;
}

fn audio_of(s: &Session, c: ClipId) -> AudioClip {
    match &s.project().clip(c).unwrap().content {
        ClipContent::Audio(a) => a.clone(),
        _ => panic!("audio clip"),
    }
}

/// Wait for transient detection.
fn analyse(s: &mut Session) {
    s.dispatch(Action::SetEditFlag(EditFlag::ShowTransients, true))
        .unwrap();
    for _ in 0..500 {
        s.tick(0.01);
        if !s.analysing_transients() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("transient detection did not finish");
}

#[test]
fn selected_clips_move_trim_and_gain_together_and_drags_are_lossless() {
    let (mut s, t, src) = setup();
    let a = add_clip(&mut s, t, src, 0.0, 2.0);
    let b = add_clip(&mut s, t, src, 4.0, 2.0);
    let both = vec![a, b];
    let q = |bars: f64| bar(bars).ticks();
    // A move drag: every step is relative to the start, one undo step.
    s.dispatch(Action::BeginGesture("Move".into())).unwrap();
    for by in [0.5, 1.0, 1.0] {
        s.dispatch(Action::MoveClips {
            clips: both.clone(),
            by: q(by),
            tracks: 0,
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(layout(&s, t), vec![(1.0, 3.0), (5.0, 7.0)]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (4.0, 6.0)]);
    // Never before zero.
    s.dispatch(Action::MoveClips {
        clips: both.clone(),
        by: -q(3.0),
        tracks: 0,
    })
    .unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (4.0, 6.0)]);
    // Trimming both ends in, then dragging back restores the audio.
    s.dispatch(Action::BeginGesture("Trim".into())).unwrap();
    for by in [-1.5, -1.0, 0.0] {
        s.dispatch(Action::TrimClips {
            clips: both.clone(),
            edge: ClipEdge::End,
            by: q(by),
            stretch: false,
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 2.0), (4.0, 6.0)]);
    // Clip gain: relative within a gesture, cumulative across gestures.
    s.dispatch(Action::BeginGesture("Gain".into())).unwrap();
    for d in [2.0, 6.0] {
        s.dispatch(Action::ClipGain {
            clips: both.clone(),
            delta_db: d,
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    s.dispatch(Action::ClipGain {
        clips: vec![b],
        delta_db: -1.0,
    })
    .unwrap();
    assert_eq!(audio_of(&s, a).gain_db, 6.0);
    assert_eq!(audio_of(&s, b).gain_db, 5.0);
    s.dispatch(Action::SetClipsGain {
        clips: both.clone(),
        db: 0.0,
    })
    .unwrap();
    assert_eq!(audio_of(&s, b).gain_db, 0.0);
    // Fades: length, shape and a drawn bend on every clip.
    s.dispatch(Action::SetFade {
        clips: both.clone(),
        edge: ClipEdge::Start,
        length: Some(4_800),
        shape: Some(faderframe_project::FadeShape::EqualPower),
        bend: Some(40),
    })
    .unwrap();
    s.dispatch(Action::SetFade {
        clips: vec![a],
        edge: ClipEdge::End,
        length: Some(i64::MAX),
        shape: None,
        bend: Some(-300),
    })
    .unwrap();
    let f = audio_of(&s, a).fades;
    assert_eq!(
        (f.fade_in, f.fade_in_shape, f.fade_in_bend, f.fade_out_bend),
        (4_800, faderframe_project::FadeShape::EqualPower, 40, -100)
    );
    assert_eq!(
        f.fade_in + f.fade_out,
        audio_of(&s, a).length,
        "fades fit the clip"
    );
    assert_eq!(audio_of(&s, b).fades.fade_in, 4_800);
    s.dispatch(Action::SetClipsMuted {
        clips: both.clone(),
        muted: true,
    })
    .unwrap();
    assert!(s.project().clip(b).unwrap().muted);
}

#[test]
fn time_stretch_trims_keep_the_content() {
    let (mut s, t, src) = setup();
    let c = add_clip(&mut s, t, src, 0.0, 2.0);
    let before = audio_of(&s, c);
    s.dispatch(Action::StretchClip {
        clip: c,
        edge: ClipEdge::End,
        to: bar(4.0),
    })
    .unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 4.0)]);
    let a = audio_of(&s, c);
    assert_eq!(a.length, before.length * 2);
    assert_eq!(a.source_span(), before.length, "same audio, twice as long");
    assert_eq!(
        a.source_at(a.length as f64 / 2.0),
        (before.source_offset + before.length / 2) as f64
    );
    // Trimming a stretched clip keeps its time map.
    s.dispatch(Action::TrimClip {
        clip: c,
        edge: ClipEdge::End,
        to: bar(2.0),
    })
    .unwrap();
    let a = audio_of(&s, c);
    assert_eq!(a.source_span(), before.length / 2);
    // Removing the warp plays the remaining audio at its own speed.
    s.dispatch(Action::ClearWarp(vec![c])).unwrap();
    assert_eq!(layout(&s, t), vec![(0.0, 1.0)]);
}

#[test]
fn warp_markers_move_transients_locally() {
    let (mut s, t, src) = setup();
    let c = add_clip(&mut s, t, src, 0.0, 2.0);
    analyse(&mut s);
    let a = audio_of(&s, c);
    let hits = s.source_transients(src).unwrap();
    let inside: Vec<i64> = hits
        .iter()
        .copied()
        .filter(|h| *h > a.source_offset + 2_000 && *h < a.source_offset + a.length - 2_000)
        .collect();
    assert!(inside.len() >= 6, "a drum loop has hits: {inside:?}");
    let hit = inside[inside.len() / 2];
    let (prev, next) = (inside[inside.len() / 2 - 1], inside[inside.len() / 2 + 1]);
    // Move one hit 10 ms later: its neighbours stay put.
    let to = hit - a.source_offset + 480;
    s.dispatch(Action::BeginGesture("Warp".into())).unwrap();
    for step in [100, 300, 480] {
        s.dispatch(Action::WarpTo {
            clip: c,
            source: hit,
            to: hit - a.source_offset + step,
            drag: faderframe_session::warping::WarpDrag::Transients,
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    let w = audio_of(&s, c);
    assert_eq!(w.length, a.length, "the clip keeps its length");
    let warp = w.warp.clone().unwrap();
    assert_eq!(
        warp.markers.len(),
        3,
        "the hit and its two neighbours: {:?}",
        warp.markers
    );
    let out = |src: i64| warp.output_of(w.source_offset, w.length, src);
    assert_eq!(out(hit), to);
    assert_eq!(out(prev), prev - a.source_offset);
    assert_eq!(out(next), next - a.source_offset);
    // Outside the neighbours nothing moved.
    let early = a.source_offset + 1_000;
    assert_eq!(out(early), 1_000);
    // Telescoping a marker lengthens the clip; removing markers resets.
    s.dispatch(Action::WarpTo {
        clip: c,
        source: hit,
        to: to + 4_800,
        drag: faderframe_session::warping::WarpDrag::Telescope,
    })
    .unwrap();
    assert_eq!(audio_of(&s, c).length, a.length + 4_800);
    for m in [hit, prev, next] {
        s.dispatch(Action::RemoveWarpMarker { clip: c, source: m })
            .unwrap();
    }
    assert!(audio_of(&s, c).warp.unwrap().markers.is_empty());
    // Quantizing pins the hits to the grid.
    s.dispatch(Action::SetGrid(GridDivision::Note(16))).unwrap();
    s.dispatch(Action::ClearWarp(vec![c])).unwrap();
    s.dispatch(Action::QuantizeWarp(vec![c])).unwrap();
    let q = audio_of(&s, c).warp.unwrap();
    let sixteenth = SR as i64 / 8;
    assert!(!q.markers.is_empty());
    assert!(
        q.markers.iter().all(|m| m.at % sixteenth == 0),
        "{:?}",
        q.markers
    );
    // Separating at transients makes one clip per hit.
    let before = s.project().clips_of(t).len();
    s.dispatch(Action::ClearWarp(vec![c])).unwrap();
    s.dispatch(Action::SeparateAtTransients(vec![c])).unwrap();
    assert!(s.project().clips_of(t).len() >= before + inside.len());
}
