//! Ripple editing: per track and global, clips and automation in sync,
//! the timeline's parts, left-out and frozen tracks, lossless drags.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
};
use faderframe_core::{AudioSourceId, AutomationLaneId, ClipId, MarkerId, SectionId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Freeze, Marker, MusicalRange,
    Project, Section, SourceSpec, StretchSettings, TrackColor, TrackKind,
};
use faderframe_session::{
    Action, ClipEdge, EditMode, EditRange, NudgeTarget, NudgeValue, RippleOp, RippleScope,
    RippleSettings, SelectMode, Session, SnapSkip,
};
use faderframe_timeline::{GridDivision, MusicalTime, TempoCurve, TempoPoint};

const SR: u32 = 48_000;
const SOURCE: AudioSourceId = AudioSourceId(9000);

fn bar(n: f64) -> MusicalTime {
    // 120 BPM, 4/4.
    MusicalTime::from_quarters(4.0 * n)
}

fn bars(t: MusicalTime) -> f64 {
    (t.quarters() / 4.0 * 1000.0).round() / 1000.0
}

/// A session with `n` audio tracks and a 32-bar source; the first track
/// selected.
fn setup(n: usize) -> (Session, Vec<TrackId>) {
    let mut s = Session::new(Project::new("Ripple", SR), None, EngineConfig::default()).unwrap();
    let tracks: Vec<TrackId> = (0..n)
        .map(|_| s.add_track(TrackKind::Audio).unwrap())
        .collect();
    s.edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: SOURCE,
            name: "loop".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 32,
                    seed: 1,
                },
            },
        }),
    })
    .unwrap();
    select_tracks(&mut s, &tracks[..1]);
    (s, tracks)
}

fn select_tracks(s: &mut Session, tracks: &[TrackId]) {
    s.dispatch(Action::SelectTracks {
        tracks: tracks.to_vec(),
        mode: SelectMode::Replace,
    })
    .unwrap();
}

fn select_clips(s: &mut Session, clips: &[ClipId]) {
    s.dispatch(Action::SelectClips {
        clips: clips.to_vec(),
        mode: SelectMode::Replace,
    })
    .unwrap();
}

/// An audio clip from `start` lasting `len` bars (its audio from the same
/// place in the source, so content can be told apart).
fn clip(s: &mut Session, t: TrackId, start: f64, len: f64) -> ClipId {
    let id = ClipId(10_000 + s.project().clips.len() as u64);
    let frame = |b: f64| (b * 4.0 * 0.5 * SR as f64) as i64;
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "c".into(),
            color: None,
            start: bar(start),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: SOURCE,
                source_offset: frame(start),
                length: frame(len),
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
    })
    .unwrap();
    id
}

/// (start, end) in bars of the track's clips, sorted.
fn layout(s: &Session, t: TrackId) -> Vec<(f64, f64)> {
    let p = s.project();
    let mut v: Vec<(f64, f64)> = p
        .clips_of(t)
        .iter()
        .map(|c| (bars(c.start), bars(c.end(&p.timeline, p.sample_rate))))
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// The source frame a track plays at `at` (bars), if any.
fn source_at(s: &Session, t: TrackId, at: f64) -> Option<i64> {
    let p = s.project();
    let rate = SR as f64;
    p.clips_of(t).iter().find_map(|c| {
        let ClipContent::Audio(a) = &c.content else {
            return None;
        };
        let start = p.timeline.to_samples(c.start, rate);
        let here = p.timeline.to_samples(bar(at), rate);
        (here >= start && here < start + a.length).then(|| a.source_offset + here - start)
    })
}

/// A volume lane on `t` through `(bar, dB)` points.
fn volume(s: &mut Session, t: TrackId, points: &[(f64, f64)]) {
    let id = AutomationLaneId(70_000 + s.project().tracks.len() as u64 * 10 + points.len() as u64);
    s.edit(Command::AddAutomationLane {
        track: t,
        lane: Box::new(AutomationLane {
            id,
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::from_points(
                points
                    .iter()
                    .map(|&(b, v)| AutomationPoint {
                        time: bar(b),
                        value: v,
                        shape: Default::default(),
                    })
                    .collect(),
            ),
            mode: AutomationMode::Read,
            visible: true,
        }),
    })
    .unwrap();
}

fn volume_at(s: &Session, t: TrackId, at: f64) -> f64 {
    s.project()
        .track(t)
        .and_then(|t| t.automation.lane(AutomationTarget::TrackVolume))
        .and_then(|l| l.curve.value_at(bar(at)))
        .unwrap()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn ripple(s: &mut Session, scope: RippleScope, on: bool) {
    let settings = RippleSettings {
        on,
        scope,
        ..s.ripple_settings()
    };
    s.dispatch(Action::Ripple(RippleOp::Settings(settings)))
        .unwrap();
}

fn marker(s: &mut Session, at: f64) -> MarkerId {
    let id = MarkerId(60_000 + s.project().markers.len() as u64);
    s.edit(Command::AddMarker {
        marker: Marker {
            id,
            position: bar(at),
            name: "M".into(),
        },
    })
    .unwrap();
    id
}

/// `apply_ripple_offset`: one track (its clips and automation; a clip
/// across the cut split there) or everything with the timeline, one undo
/// step either way.
#[test]
fn an_offset_moves_one_track_or_everything() {
    let (mut s, t) = setup(2);
    clip(&mut s, t[0], 0.0, 4.0);
    clip(&mut s, t[0], 6.0, 1.0);
    clip(&mut s, t[1], 3.0, 2.0);
    volume(&mut s, t[0], &[(0.0, 0.0), (8.0, -8.0)]);
    let m = marker(&mut s, 5.0);
    let before = s.project().clone();
    s.apply_ripple_offset(bar(2.0), bar(1.0), Some(t[0]))
        .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 2.0), (3.0, 5.0), (7.0, 8.0)]);
    assert_eq!(source_at(&s, t[0], 3.0), source_at_of(&before, t[0], 2.0));
    assert_eq!(layout(&s, t[1]), vec![(3.0, 5.0)], "another track stays");
    assert!(
        near(volume_at(&s, t[0], 2.5), -2.0),
        "the gap holds the value at the cut"
    );
    assert!(
        near(volume_at(&s, t[0], 5.0), -4.0),
        "the curve after it moved along"
    );
    assert_eq!(
        s.project().markers[0].position,
        bar(5.0),
        "the timeline stays"
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips, before.clips);
    // Everything: every track, the markers too.
    s.apply_ripple_offset(bar(4.0), MusicalTime(-bar(1.0).ticks()), None)
        .unwrap();
    assert_eq!(
        layout(&s, t[0]),
        vec![(0.0, 3.0), (5.0, 6.0)],
        "[3, 4) went"
    );
    assert_eq!(layout(&s, t[1]), vec![(3.0, 4.0)], "[4, 5) is what is left");
    let mk = s.project().markers.iter().find(|x| x.id == m).unwrap();
    assert_eq!(mk.position, bar(4.0));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips, before.clips);
    assert_eq!(s.project().markers, before.markers);
}

fn source_at_of(p: &Project, t: TrackId, at: f64) -> Option<i64> {
    let rate = SR as f64;
    p.clips_of(t).iter().find_map(|c| {
        let ClipContent::Audio(a) = &c.content else {
            return None;
        };
        let start = p.timeline.to_samples(c.start, rate);
        let here = p.timeline.to_samples(bar(at), rate);
        (here >= start && here < start + a.length).then(|| a.source_offset + here - start)
    })
}

/// A Shuffle trim moves what follows and the automation with it, by the
/// same amount at the same place.
#[test]
fn the_automation_moves_in_sync_with_the_clips() {
    let (mut s, t) = setup(1);
    let a = clip(&mut s, t[0], 0.0, 4.0);
    clip(&mut s, t[0], 4.0, 2.0);
    volume(&mut s, t[0], &[(0.0, 0.0), (4.0, -12.0), (5.0, -3.0)]);
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    s.dispatch(Action::TrimClip {
        clip: a,
        edge: ClipEdge::End,
        to: bar(3.0),
    })
    .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 3.0), (3.0, 5.0)]);
    // What played at 4 and 5 bars now plays at 3 and 4, with its level.
    assert!(near(volume_at(&s, t[0], 3.0), -12.0));
    assert!(near(volume_at(&s, t[0], 4.0), -3.0));
    assert!(
        near(volume_at(&s, t[0], 1.5), -4.5),
        "before the cut it is as it was"
    );
    // Longer again: the gap holds the value reached at the old end.
    s.dispatch(Action::TrimClip {
        clip: a,
        edge: ClipEdge::End,
        to: bar(5.0),
    })
    .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 5.0), (5.0, 7.0)]);
    assert!(near(volume_at(&s, t[0], 5.0), -12.0));
}

/// The scopes: only the edited track, the selected tracks with it, or
/// every track with the timeline; the tracks kept in sync are cut.
#[test]
fn scopes_choose_the_tracks_and_the_others_are_cut() {
    for scope in RippleScope::ALL {
        let (mut s, t) = setup(3);
        let a = clip(&mut s, t[0], 0.0, 2.0);
        clip(&mut s, t[0], 2.0, 2.0);
        for &other in &t[1..] {
            clip(&mut s, other, 0.0, 8.0);
        }
        let m = marker(&mut s, 6.0);
        select_tracks(&mut s, &t[..2]);
        ripple(&mut s, scope, true);
        // Ripple on in Grid mode: the trim snaps and ripples.
        s.dispatch(Action::TrimClip {
            clip: a,
            edge: ClipEdge::End,
            to: bar(1.0),
        })
        .unwrap();
        assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (1.0, 3.0)], "{scope:?}");
        let cut = vec![(0.0, 1.0), (1.0, 7.0)];
        let whole = vec![(0.0, 8.0)];
        let (second, third, moved) = match scope {
            RippleScope::Edited => (&whole, &whole, false),
            RippleScope::Selected => (&cut, &whole, false),
            RippleScope::All => (&cut, &cut, true),
        };
        assert_eq!(&layout(&s, t[1]), second, "{scope:?}: the selected track");
        assert_eq!(&layout(&s, t[2]), third, "{scope:?}: the other track");
        let mk = s.project().markers.iter().find(|x| x.id == m).unwrap();
        assert_eq!(mk.position == bar(5.0), moved, "{scope:?}: the marker");
        // What a track kept in sync plays after the cut is what played
        // a bar later.
        if scope != RippleScope::Edited {
            assert_eq!(
                source_at(&s, t[1], 1.5),
                Some((2.5 * 4.0 * 0.5 * SR as f64) as i64)
            );
        }
    }
}

/// Deleting clips: their time closes on their own tracks (Edited), or
/// the time of all of them everywhere (All Tracks).
#[test]
fn ripple_deletes_close_per_track_or_everywhere() {
    let (mut s, t) = setup(2);
    let a = clip(&mut s, t[0], 1.0, 1.0);
    clip(&mut s, t[0], 3.0, 1.0);
    let b = clip(&mut s, t[1], 5.0, 1.0);
    clip(&mut s, t[1], 7.0, 1.0);
    let before = s.project().clone();
    select_clips(&mut s, &[a, b]);
    s.dispatch(Action::Ripple(RippleOp::DeleteClips(RippleScope::Edited)))
        .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(2.0, 3.0)]);
    assert_eq!(layout(&s, t[1]), vec![(6.0, 7.0)]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips, before.clips);
    select_clips(&mut s, &[a, b]);
    s.dispatch(Action::Ripple(RippleOp::DeleteClips(RippleScope::All)))
        .unwrap();
    // Both clips' time went from both tracks.
    assert_eq!(layout(&s, t[0]), vec![(2.0, 3.0)]);
    assert_eq!(layout(&s, t[1]), vec![(5.0, 6.0)]);
    s.dispatch(Action::Undo).unwrap();
    // The Delete key ripples when rippling is on.
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    select_clips(&mut s, &[a]);
    s.dispatch(Action::DeleteInArranger).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(2.0, 3.0)]);
    assert_eq!(layout(&s, t[1]), vec![(5.0, 6.0), (7.0, 8.0)]);
}

/// A range taken out of all tracks takes the timeline's parts along:
/// markers, sections, the loop, tempo changes — as the settings say.
#[test]
fn a_global_ripple_moves_the_timeline_as_chosen() {
    let (mut s, t) = setup(2);
    clip(&mut s, t[0], 0.0, 8.0);
    clip(&mut s, t[1], 6.0, 2.0);
    let m = marker(&mut s, 6.0);
    s.edit(Command::AddSection {
        section: Section {
            id: SectionId(61_000),
            name: "Chorus".into(),
            start: bar(4.0),
            end: bar(8.0),
            color: TrackColor { r: 1, g: 2, b: 3 },
        },
    })
    .unwrap();
    s.edit(Command::SetLoop {
        range: Some(MusicalRange {
            start: bar(4.0),
            end: bar(6.0),
        }),
        enabled: true,
    })
    .unwrap();
    let mut tl = s.project().timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: bar(6.0),
        bpm: 90.0,
        curve: TempoCurve::Constant,
    });
    s.edit(Command::SetTimeline {
        timeline: Box::new(tl),
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(3.0),
    ))))
    .unwrap();
    s.dispatch(Action::Ripple(RippleOp::DeleteRange(RippleScope::All)))
        .unwrap();
    let p = s.project();
    assert_eq!(
        p.markers.iter().find(|x| x.id == m).unwrap().position,
        bar(4.0)
    );
    assert_eq!(
        (p.sections[0].start, p.sections[0].end),
        (bar(2.0), bar(6.0))
    );
    assert_eq!(
        p.loop_range.map(|r| (r.start, r.end)),
        Some((bar(2.0), bar(4.0)))
    );
    assert_eq!(p.timeline.tempo.bpm_at(bar(4.5)), 90.0);
    assert_eq!(p.timeline.tempo.bpm_at(bar(3.5)), 120.0);
    assert_eq!(
        layout(&s, t[1]),
        vec![(4.0, 5.5)],
        "audio at 90 bpm lasts longer"
    );
    s.dispatch(Action::Undo).unwrap();
    // Without the markers and the tempo: only the tracks move.
    let settings = RippleSettings {
        markers: false,
        tempo: false,
        ..s.ripple_settings()
    };
    s.dispatch(Action::Ripple(RippleOp::Settings(settings)))
        .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(3.0),
    ))))
    .unwrap();
    s.dispatch(Action::Ripple(RippleOp::DeleteRange(RippleScope::All)))
        .unwrap();
    let p = s.project();
    assert_eq!(
        p.markers.iter().find(|x| x.id == m).unwrap().position,
        bar(6.0)
    );
    assert_eq!(p.timeline.tempo.bpm_at(bar(6.5)), 90.0);
    assert_eq!(
        layout(&s, t[0]),
        vec![(0.0, 1.0), (1.0, 6.0)],
        "cut at both ends"
    );
}

/// A track left out of rippling stays; a frozen one too, with a notice.
#[test]
fn left_out_and_frozen_tracks_stay() {
    let (mut s, t) = setup(3);
    let a = clip(&mut s, t[0], 0.0, 2.0);
    clip(&mut s, t[1], 3.0, 1.0);
    clip(&mut s, t[2], 3.0, 1.0);
    s.dispatch(Action::Ripple(RippleOp::Exclude {
        tracks: vec![t[1]],
        excluded: true,
    }))
    .unwrap();
    assert!(s.project().track(t[1]).unwrap().ripple_excluded);
    s.edit(Command::SetTrackFreeze {
        track: t[2],
        freeze: Some(Freeze {
            source: SOURCE,
            start: MusicalTime::ZERO,
            length: 96_000,
            latency: 0,
        }),
    })
    .unwrap();
    ripple(&mut s, RippleScope::All, true);
    s.dispatch(Action::TrimClip {
        clip: a,
        edge: ClipEdge::End,
        to: bar(1.0),
    })
    .unwrap();
    assert_eq!(layout(&s, t[1]), vec![(3.0, 4.0)], "left out");
    assert_eq!(layout(&s, t[2]), vec![(3.0, 4.0)], "frozen");
    assert!(
        s.notices()
            .any(|n| n.text.contains("frozen: left out of the ripple")),
        "{:?}",
        s.notices().map(|n| &n.text).collect::<Vec<_>>()
    );
    // Its own edits still ripple it.
    s.dispatch(Action::Ripple(RippleOp::Exclude {
        tracks: vec![t[1]],
        excluded: false,
    }))
    .unwrap();
    assert!(!s.project().track(t[1]).unwrap().ripple_excluded);
}

/// A ripple drag is worked out afresh each motion: dragging back leaves
/// the project as it was although motions split clips on the way; the
/// drag is one undo step; what it moves is no snap target.
#[test]
fn a_ripple_drag_is_lossless_and_one_undo_step() {
    let (mut s, t) = setup(2);
    let a = clip(&mut s, t[0], 0.0, 2.0);
    let b = clip(&mut s, t[0], 3.0, 1.0);
    clip(&mut s, t[1], 0.0, 8.0);
    volume(&mut s, t[1], &[(0.0, 0.0), (8.0, -8.0)]);
    ripple(&mut s, RippleScope::All, true);
    let before = s.project().clone();
    let steps = s.history_steps().0.len();
    s.dispatch(Action::BeginGesture("Trim Clip".into()))
        .unwrap();
    for by in [-1.0, -1.5, 0.5, 1.0, -0.5] {
        s.dispatch(Action::TrimClips {
            clips: vec![a],
            edge: ClipEdge::End,
            by: bar(by).ticks(),
            stretch: false,
        })
        .unwrap();
        let end = 2.0 + by;
        assert_eq!(layout(&s, t[0])[1], (3.0 + by, 4.0 + by), "by {by}");
        assert_eq!(
            layout(&s, t[1]).len(),
            2,
            "the other track is cut once, by {by}"
        );
        assert!(near(
            volume_at(&s, t[1], end.min(2.0) - 0.25),
            -(end.min(2.0) - 0.25)
        ));
    }
    // Pushed along: not a snap target while it moves.
    let b_start = s.project().clip(b).unwrap().start;
    let hit = s.snap_to_object(b_start, bar(0.1), &SnapSkip::clips(&[a]));
    assert!(
        hit.is_none_or(|h| h.object != faderframe_session::SnapObject::ClipStart(b)),
        "{hit:?}"
    );
    s.dispatch(Action::TrimClips {
        clips: vec![a],
        edge: ClipEdge::End,
        by: 0,
        stretch: false,
    })
    .unwrap();
    assert_eq!(s.project().clips, before.clips, "back where it started");
    assert_eq!(
        s.project().track(t[1]).unwrap().automation,
        before.track(t[1]).unwrap().automation
    );
    s.dispatch(Action::TrimClips {
        clips: vec![a],
        edge: ClipEdge::End,
        by: bar(-1.0).ticks(),
        stretch: false,
    })
    .unwrap();
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips, before.clips);
    assert_eq!(
        s.project().track(t[1]).unwrap().automation,
        before.track(t[1]).unwrap().automation
    );
}

/// Ripple on in Grid mode: a move pushes what follows (those it passes
/// over stay), nudges and Spot push too.
#[test]
fn moves_push_what_follows() {
    let (mut s, t) = setup(1);
    let a = clip(&mut s, t[0], 2.0, 1.0);
    clip(&mut s, t[0], 4.0, 1.0);
    clip(&mut s, t[0], 0.0, 1.0);
    volume(&mut s, t[0], &[(0.0, 0.0), (8.0, -8.0)]);
    ripple(&mut s, RippleScope::Edited, true);
    s.dispatch(Action::BeginGesture("Move Clip".into()))
        .unwrap();
    s.dispatch(Action::MoveClips {
        clips: vec![a],
        by: bar(1.0).ticks(),
        tracks: 0,
    })
    .unwrap();
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (3.0, 4.0), (5.0, 6.0)]);
    assert!(
        near(volume_at(&s, t[0], 3.0), -2.0),
        "the automation came along"
    );
    s.dispatch(Action::SetNudge(NudgeValue::Grid(GridDivision::Bar)))
        .unwrap();
    select_clips(&mut s, &[a]);
    s.dispatch(Action::Nudge {
        forward: false,
        target: NudgeTarget::Move,
    })
    .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (2.0, 3.0), (4.0, 5.0)]);
    s.dispatch(Action::SpotClip {
        clip: a,
        start: bar(1.0),
    })
    .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (1.0, 2.0), (3.0, 4.0)]);
}

/// Paste and duplicate insert when rippling, on the tracks kept in sync
/// too.
#[test]
fn duplicates_open_room_on_every_track() {
    let (mut s, t) = setup(2);
    let a = clip(&mut s, t[0], 0.0, 1.0);
    clip(&mut s, t[0], 1.0, 1.0);
    clip(&mut s, t[1], 0.0, 4.0);
    let m = marker(&mut s, 2.0);
    ripple(&mut s, RippleScope::All, true);
    select_clips(&mut s, &[a]);
    s.dispatch(Action::Repeat(1)).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)]);
    assert_eq!(layout(&s, t[1]), vec![(0.0, 1.0), (2.0, 5.0)]);
    assert_eq!(
        s.project()
            .markers
            .iter()
            .find(|x| x.id == m)
            .unwrap()
            .position,
        bar(3.0)
    );
}

/// Remove Gaps: between the selected clips on their tracks, or the time no
/// track has a clip in.
#[test]
fn gaps_close_per_track_or_where_no_track_plays() {
    let (mut s, t) = setup(2);
    let a = clip(&mut s, t[0], 0.0, 1.0);
    let b = clip(&mut s, t[0], 2.0, 1.0);
    let c = clip(&mut s, t[0], 5.0, 1.0);
    clip(&mut s, t[1], 3.5, 1.0);
    select_clips(&mut s, &[a, b, c]);
    s.dispatch(Action::Ripple(RippleOp::RemoveGaps(RippleScope::Edited)))
        .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)]);
    assert_eq!(layout(&s, t[1]), vec![(3.5, 4.5)]);
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::SelectClips {
        clips: vec![],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Ripple(RippleOp::RemoveGaps(RippleScope::All)))
        .unwrap();
    // [1, 2), [3, 3.5) and [4.5, 5) were empty on every track.
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (1.0, 2.0), (3.0, 4.0)]);
    assert_eq!(layout(&s, t[1]), vec![(2.0, 3.0)]);
}

/// Trim to Selection, rippling: the clip keeps its start and what follows
/// closes up by what went from both ends.
#[test]
fn trim_to_selection_closes_up() {
    let (mut s, t) = setup(1);
    clip(&mut s, t[0], 0.0, 4.0);
    clip(&mut s, t[0], 5.0, 1.0);
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(2.0),
    ))))
    .unwrap();
    s.dispatch(Action::TrimToSelection).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (2.0, 3.0)]);
    assert_eq!(
        source_at(&s, t[0], 0.5),
        Some((1.5 * 4.0 * 0.5 * SR as f64) as i64)
    );
}

/// Insert Time in all tracks: every track opens at the range's start.
#[test]
fn time_inserted_in_all_tracks() {
    let (mut s, t) = setup(2);
    clip(&mut s, t[0], 0.0, 2.0);
    clip(&mut s, t[1], 3.0, 1.0);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(2.0),
    ))))
    .unwrap();
    s.dispatch(Action::Ripple(RippleOp::InsertTime(RippleScope::All)))
        .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (2.0, 3.0)]);
    assert_eq!(layout(&s, t[1]), vec![(4.0, 5.0)]);
}

/// Shuffle reorders carry their stretch of automation.
#[test]
fn shuffled_clips_take_their_automation() {
    let (mut s, t) = setup(1);
    let a = clip(&mut s, t[0], 0.0, 1.0);
    clip(&mut s, t[0], 1.0, 1.0);
    volume(
        &mut s,
        t[0],
        &[(0.0, -10.0), (1.0, -10.0), (1.0, 0.0), (2.0, 0.0)],
    );
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    s.dispatch(Action::ShuffleClip {
        clip: a,
        track: t[0],
        at: bar(1.9),
    })
    .unwrap();
    assert_eq!(s.project().clip(a).unwrap().start, bar(1.0));
    assert!(
        near(volume_at(&s, t[0], 0.5), 0.0),
        "the other clip's level came first"
    );
    assert!(
        near(volume_at(&s, t[0], 1.5), -10.0),
        "its own level went with it"
    );
}

/// The demo: a global ripple trim of the drums' end (Bass left out) goes
/// through on every other track.
#[test]
fn a_global_ripple_trim_of_the_demo() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let id = |s: &Session, n: &str| s.project().tracks.iter().find(|t| t.name == n).unwrap().id;
    let (drums, bass) = (id(&s, "Drums"), id(&s, "Bass"));
    ripple(&mut s, RippleScope::All, true);
    s.dispatch(Action::Ripple(RippleOp::Exclude {
        tracks: vec![bass],
        excluded: true,
    }))
    .unwrap();
    let clip = s.project().clips_of(drums)[0].id;
    let end = |s: &Session| {
        let p = s.project();
        p.clip(clip).unwrap().end(&p.timeline, p.sample_rate)
    };
    let before = end(&s);
    s.dispatch(Action::BeginGesture("Trim Clip".into()))
        .unwrap();
    for q in [-1.0, -2.0, -4.0, -5.0] {
        let r = s.dispatch(Action::TrimClips {
            clips: vec![clip],
            edge: ClipEdge::End,
            by: MusicalTime::from_quarters(q).ticks(),
            stretch: false,
        });
        assert!(r.is_ok(), "{q}: {r:?}");
        let moved = (end(&s) - before).quarters();
        assert!((moved - q).abs() < 0.01, "{q}: moved {moved}");
    }
    s.dispatch(Action::EndGesture).unwrap();
}
