//! Time-locked tracks: tempo changes on every path keep their clips and
//! automation on their samples (one undo step with the change), musical
//! tracks move with the beats; edits place locked material on whole
//! samples; ripples move it by the time they take out; section moves
//! carry it with its section.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
};
use faderframe_core::{AudioSourceId, AutomationLaneId, ClipId, MarkerId, SectionId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Marker, MidiClip, Project,
    Section, SourceSpec, StretchSettings, TimeBase, TrackColor, TrackKind,
};
use faderframe_session::hits::{HitRequest, HitSource};
use faderframe_session::{
    Action, ClipEdge, EditRange, RippleOp, RippleScope, RippleSettings, SelectMode, Session,
    TransportAction,
};
use faderframe_timeline::{MusicalTime, TempoCurve, TempoPoint};

const SR: u32 = 48_000;
const SOURCE: AudioSourceId = AudioSourceId(9000);

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

/// Locked audio (clips at bars 2.3 and 5, a volume lane), locked
/// instrument (a MIDI clip at bar 3.1) and musical audio (a clip at bar 5)
/// tracks, at 120 BPM.
struct Fixture {
    s: Session,
    locked: TrackId,
    keys: TrackId,
    musical: TrackId,
    a: ClipId,
    b: ClipId,
    m: ClipId,
    free: ClipId,
}

fn session() -> Session {
    let mut s = Session::new(Project::new("Lock", SR), None, EngineConfig::default()).unwrap();
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
    s
}

fn audio(s: &mut Session, t: TrackId, start: MusicalTime, frames: i64) -> ClipId {
    let id = ClipId(10_000 + s.project().clips.len() as u64);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: format!("clip {}", id.0),
            color: None,
            start,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: SOURCE,
                source_offset: 0,
                length: frames,
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

fn midi(s: &mut Session, t: TrackId, start: MusicalTime) -> ClipId {
    let id = ClipId(10_000 + s.project().clips.len() as u64);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "cue".into(),
            color: None,
            start,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: bar(1.0),
                ..MidiClip::default()
            }),
        }),
    })
    .unwrap();
    id
}

fn volume(s: &mut Session, t: TrackId, points: &[(f64, f64)]) {
    s.edit(Command::AddAutomationLane {
        track: t,
        lane: Box::new(AutomationLane {
            id: AutomationLaneId(70_000 + t.0),
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

fn lock(s: &mut Session, t: TrackId) {
    s.dispatch(Action::Edit(Command::SetTrackTimeBase {
        track: t,
        base: TimeBase::TimeLocked,
    }))
    .unwrap();
}

fn fixture() -> Fixture {
    let mut s = session();
    let locked = s.add_track(TrackKind::Audio).unwrap();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    let musical = s.add_track(TrackKind::Audio).unwrap();
    let a = audio(&mut s, locked, bar(2.3), 24_000);
    let b = audio(&mut s, locked, bar(5.0), 24_000);
    let m = midi(&mut s, keys, bar(3.1));
    let free = audio(&mut s, musical, bar(5.0), 24_000);
    volume(&mut s, locked, &[(0.5, 0.0), (4.25, -6.0), (7.0, -12.0)]);
    lock(&mut s, locked);
    lock(&mut s, keys);
    Fixture {
        s,
        locked,
        keys,
        musical,
        a,
        b,
        m,
        free,
    }
}

fn sample(s: &Session, t: MusicalTime) -> i64 {
    s.project().timeline.to_samples(t, f64::from(SR))
}

fn start(s: &Session, c: ClipId) -> i64 {
    sample(s, s.project().clip(c).unwrap().start)
}

/// Where the locked material sounds: clip starts and automation points.
fn places(f: &Fixture) -> Vec<i64> {
    let s = &f.s;
    let mut v: Vec<i64> = [f.a, f.b, f.m].iter().map(|c| start(s, *c)).collect();
    v.extend(
        s.project().track(f.locked).unwrap().automation.lanes[0]
            .curve
            .points()
            .iter()
            .map(|p| sample(s, p.time)),
    );
    v
}

fn undo(s: &mut Session) {
    s.dispatch(Action::Undo).unwrap();
}

/// The undo steps.
fn steps(s: &Session) -> usize {
    s.history_steps().0.len()
}

/// Every tempo-lane edit, Set Tempo (what the tempo from a clip sets) and
/// a whole new tempo map keep the locked tracks where they sound, the
/// musical one moves; each is one undo step that puts everything back.
#[test]
fn tempo_changes_keep_time_locked_tracks_in_time() {
    let mut f = fixture();
    let before = places(&f);
    let free_before = start(&f.s, f.free);
    let check = |f: &mut Fixture, what: &str, edit: &dyn Fn(&mut Session)| {
        let project = f.s.project().clone();
        let n = steps(&f.s);
        edit(&mut f.s);
        assert_eq!(places(f), before, "{what}: the locked material stays");
        assert_eq!(steps(&f.s), n + 1, "{what}: one undo step");
        let p = f.s.project();
        assert_eq!(p.clip(f.free).unwrap().start, bar(5.0), "{what}");
        undo(&mut f.s);
        assert_eq!(f.s.project().clips, project.clips, "{what}: undone");
        assert_eq!(f.s.project().tracks, project.tracks, "{what}: undone");
        assert_eq!(f.s.project().timeline, project.timeline, "{what}: undone");
        f.s.dispatch(Action::Redo).unwrap();
        assert_eq!(places(f), before, "{what}: redone");
    };
    // A tempo point keeps the tempo there: nothing moves yet.
    f.s.dispatch(Action::AddTempoPoint(bar(1.5))).unwrap();
    assert_eq!(places(&f), before);
    check(&mut f, "a tempo point's tempo", &|s| {
        s.dispatch(Action::SetTempoPoint {
            index: 1,
            position: bar(1.5),
            bpm: 93.0,
        })
        .unwrap();
    });
    // A drag of a point: many tempo maps in one gesture.
    check(&mut f, "a drag", &|s| {
        s.dispatch(Action::BeginGesture("Tempo".into())).unwrap();
        for (i, bpm) in [100.0, 87.5, 141.25, 66.6, 93.0].into_iter().enumerate() {
            s.dispatch(Action::SetTempoPoint {
                index: 1,
                position: bar(1.0 + 0.25 * i as f64),
                bpm,
            })
            .unwrap();
        }
        s.dispatch(Action::EndGesture).unwrap();
    });
    assert_ne!(start(&f.s, f.free), free_before, "the musical clip moved");
    check(&mut f, "a ramp", &|s| {
        s.dispatch(Action::SetTempoRamp {
            index: 0,
            ramp: true,
        })
        .unwrap();
    });
    check(&mut f, "a point removed", &|s| {
        s.dispatch(Action::RemoveTempoPoint(1)).unwrap();
    });
    check(&mut f, "Set Tempo", &|s| {
        s.dispatch(Action::Edit(Command::SetTempo { bpm: 71.3 }))
            .unwrap();
    });
    check(&mut f, "a new tempo map", &|s| {
        let mut tl = s.project().timeline.clone();
        tl.tempo = faderframe_timeline::TempoMap::new(133.0);
        tl.tempo.set_point(TempoPoint {
            position: bar(4.0),
            bpm: 61.0,
            curve: TempoCurve::Constant,
        });
        s.dispatch(Action::Edit(Command::SetTimeline {
            timeline: Box::new(tl),
        }))
        .unwrap();
    });
    // The MIDI clip's notes follow the tempo: its length stays a bar.
    assert_eq!(
        f.s.project().clip(f.m).unwrap().as_midi().unwrap().length,
        bar(1.0)
    );
    // Back to musical: tempo changes move it again.
    f.s.dispatch(Action::Edit(Command::SetTrackTimeBase {
        track: f.locked,
        base: TimeBase::Musical,
    }))
    .unwrap();
    let at = f.s.project().clip(f.a).unwrap().start;
    f.s.dispatch(Action::Edit(Command::SetTempo { bpm: 100.0 }))
        .unwrap();
    assert_eq!(f.s.project().clip(f.a).unwrap().start, at);
    assert_ne!(start(&f.s, f.a), before[0]);
    let _ = (f.keys, f.musical);
}

/// Tempo through hit points: the locked tracks keep their samples whether
/// or not audio is kept where it sounds; a musical audio clip follows the
/// beats when it is not.
#[test]
fn tempo_from_hit_points_keeps_time_locked_tracks() {
    let mut f = fixture();
    let secs = [1.3, 2.9, 4.1, 6.05];
    for (i, t) in secs.iter().enumerate() {
        let position = f.s.project().timeline.tempo.seconds_to_musical(*t);
        f.s.edit(Command::AddMarker {
            marker: Marker {
                id: MarkerId(500 + i as u64),
                position,
                name: format!("Hit {i}"),
            },
        })
        .unwrap();
    }
    let before = places(&f);
    let free = start(&f.s, f.free);
    for keep_audio in [false, true] {
        let n = steps(&f.s);
        f.s.dispatch(Action::TempoFromHits(HitRequest {
            source: HitSource::Markers,
            keep_audio,
            ..HitRequest::default()
        }))
        .unwrap();
        assert_ne!(f.s.project().timeline.tempo.points()[0].bpm, 120.0);
        assert_eq!(places(&f), before, "keep audio {keep_audio}");
        assert_eq!(steps(&f.s), n + 1);
        if keep_audio {
            assert!((start(&f.s, f.free) - free).abs() <= 1);
        } else {
            assert_ne!(start(&f.s, f.free), free, "follows the beats");
        }
        undo(&mut f.s);
        assert_eq!(places(&f), before);
    }
}

/// A ripple takes time out of a locked track in time: what follows moves
/// by the time taken out, its spacing kept across a tempo change; a
/// musical track moves by bars. All Tracks with the tempo: the same.
#[test]
fn ripples_move_time_locked_material_by_the_time_taken_out() {
    let mut s = session();
    let locked = s.add_track(TrackKind::Audio).unwrap();
    let musical = s.add_track(TrackKind::Audio).unwrap();
    let mut tl = s.project().timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: bar(6.0),
        bpm: 60.0,
        curve: TempoCurve::Constant,
    });
    s.edit(Command::SetTimeline {
        timeline: Box::new(tl),
    })
    .unwrap();
    let l: Vec<ClipId> = [3.0, 8.0, 10.0]
        .map(|b| audio(&mut s, locked, bar(b), 4_800))
        .to_vec();
    let m: Vec<ClipId> = [8.0, 10.0]
        .map(|b| audio(&mut s, musical, bar(b), 4_800))
        .to_vec();
    volume(&mut s, locked, &[(0.0, 0.0), (9.0, -6.0)]);
    lock(&mut s, locked);
    let taken = sample(&s, bar(2.0)) - sample(&s, bar(1.0));
    let lane_point = |s: &Session| {
        let p = s.project().track(locked).unwrap().automation.lanes[0]
            .curve
            .points()
            .last()
            .map(|p| p.time)
            .unwrap();
        sample(s, p)
    };
    let before: Vec<i64> = l.iter().map(|c| start(&s, *c)).collect();
    let lane = lane_point(&s);
    let spacing = |s: &Session, c: &[ClipId]| start(s, c[1]) - start(s, c[0]);
    let musical_spacing = spacing(&s, &m);
    for scope in [RippleScope::Edited, RippleScope::All] {
        s.dispatch(Action::SelectTracks {
            tracks: vec![locked, musical],
            mode: SelectMode::Replace,
        })
        .unwrap();
        s.dispatch(Action::Ripple(RippleOp::Settings(RippleSettings {
            scope,
            ..s.ripple_settings()
        })))
        .unwrap();
        s.dispatch(Action::SetEditRange(Some(EditRange::new(
            bar(1.0),
            bar(2.0),
        ))))
        .unwrap();
        s.dispatch(Action::Ripple(RippleOp::DeleteRange(scope)))
            .unwrap();
        let after: Vec<i64> = l.iter().map(|c| start(&s, *c)).collect();
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(*a, b - taken, "{scope:?}: by the time taken out");
        }
        assert_eq!(lane_point(&s), lane - taken, "{scope:?}: the automation");
        match scope {
            // The musical track moved by a bar: past the tempo change a
            // bar lasts twice as long, so it moved further in time.
            RippleScope::Edited => {
                assert_eq!(s.project().clip(m[0]).unwrap().start, bar(7.0));
                assert_eq!(spacing(&s, &m), musical_spacing);
                assert!(start(&s, m[0]) < sample(&s, bar(8.0)) - 2 * taken + taken / 2);
            }
            // The tempo moved along: the musical track keeps its time too.
            _ => {
                assert!((spacing(&s, &m) - musical_spacing).abs() <= 1);
                assert!((start(&s, m[0]) - (before[1] - taken)).abs() <= 1);
            }
        }
        undo(&mut s);
        let back: Vec<i64> = l.iter().map(|c| start(&s, *c)).collect();
        assert_eq!(back, before, "{scope:?}: undone");
    }
}

/// A section move carries the locked material in it (its place in the
/// section kept); what it leaves where it was keeps its sample.
#[test]
fn section_moves_carry_time_locked_material_with_their_section() {
    let mut s = session();
    let locked = s.add_track(TrackKind::Audio).unwrap();
    let mut tl = s.project().timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: bar(4.0),
        bpm: 90.0,
        curve: TempoCurve::Constant,
    });
    tl.tempo.set_point(TempoPoint {
        position: bar(8.0),
        bpm: 120.0,
        curve: TempoCurve::Constant,
    });
    s.edit(Command::SetTimeline {
        timeline: Box::new(tl),
    })
    .unwrap();
    let sections = [(0.0, 4.0), (4.0, 8.0), (8.0, 10.0)];
    for (i, (a, b)) in sections.iter().enumerate() {
        s.edit(Command::AddSection {
            section: Section {
                id: SectionId(800 + i as u64),
                name: format!("S{i}"),
                start: bar(*a),
                end: bar(*b),
                color: TrackColor::palette(i),
            },
        })
        .unwrap();
    }
    for b in [1.37, 5.21, 9.0] {
        audio(&mut s, locked, bar(b), 4_800);
    }
    lock(&mut s, locked);
    let section_start = |s: &Session, id: u64| {
        let sec = s
            .project()
            .sections
            .iter()
            .find(|x| x.id == SectionId(id))
            .unwrap();
        sample(s, sec.start)
    };
    // Clips are renumbered by a rearrangement: by their order in time.
    let starts = |s: &Session| -> Vec<i64> {
        s.project()
            .clips_of(locked)
            .iter()
            .map(|c| sample(s, c.start))
            .collect()
    };
    let at = starts(&s);
    let (early_in, late_in) = (
        at[0] - section_start(&s, 800),
        at[1] - section_start(&s, 801),
    );
    // The second section to the front: the first follows it.
    s.dispatch(Action::MoveSection {
        section: SectionId(801),
        to: MusicalTime::ZERO,
        copy: false,
    })
    .unwrap();
    assert_eq!(section_start(&s, 801), 0);
    let near = |a: i64, b: i64| (a - b).abs() <= 1;
    let now = starts(&s);
    assert!(
        near(now[0] - section_start(&s, 801), late_in),
        "carried with its section"
    );
    assert!(
        near(now[1] - section_start(&s, 800), early_in),
        "and the other one"
    );
    // Both sections took as long as before: the third is where it was.
    assert!(near(now[2], at[2]));
    undo(&mut s);
    assert_eq!(starts(&s), at);
    // A move after the third: what stays before it keeps its sample.
    s.dispatch(Action::MoveSection {
        section: SectionId(801),
        to: bar(10.0),
        copy: false,
    })
    .unwrap();
    let now = starts(&s);
    assert_eq!(now[0], at[0]);
    assert!(near(now[2] - section_start(&s, 801), late_in));
}

/// Moves, trims, splits, paste and duplicates put locked material on
/// whole samples (the grid snaps onto the sample under the line).
#[test]
fn edits_on_a_time_locked_track_land_on_whole_samples() {
    let mut s = session();
    let locked = s.add_track(TrackKind::Audio).unwrap();
    let musical = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTempo { bpm: 97.0 }).unwrap();
    let c = audio(&mut s, locked, bar(1.0), 96_000);
    let free = audio(&mut s, musical, bar(1.0), 96_000);
    lock(&mut s, locked);
    let rate = f64::from(SR);
    let on_sample = |s: &Session, t: MusicalTime| {
        let tl = &s.project().timeline;
        tl.at_sample(tl.to_samples(t, rate), rate) == t
    };
    // A grid position at 97 BPM is between samples.
    let grid = bar(2.25);
    assert!(!on_sample(&s, grid));
    s.dispatch(Action::Edit(Command::MoveClip {
        clip: c,
        track: locked,
        start: grid,
    }))
    .unwrap();
    let at = s.project().clip(c).unwrap().start;
    assert!(on_sample(&s, at));
    assert_eq!(sample(&s, at), sample(&s, grid), "the grid line's sample");
    s.dispatch(Action::Edit(Command::MoveClip {
        clip: free,
        track: musical,
        start: grid,
    }))
    .unwrap();
    assert_eq!(
        s.project().clip(free).unwrap().start,
        grid,
        "musical as given"
    );
    // A gesture's move by ticks.
    s.dispatch(Action::BeginGesture("Move".into())).unwrap();
    s.dispatch(Action::MoveClips {
        clips: vec![c],
        by: bar(0.5).ticks(),
        tracks: 0,
    })
    .unwrap();
    s.dispatch(Action::EndGesture).unwrap();
    assert!(on_sample(&s, s.project().clip(c).unwrap().start));
    // A trim of the front.
    s.dispatch(Action::TrimClip {
        clip: c,
        edge: ClipEdge::Start,
        to: bar(2.875),
    })
    .unwrap();
    let trimmed = s.project().clip(c).unwrap().clone();
    assert!(on_sample(&s, trimmed.start));
    assert_eq!(sample(&s, trimmed.start), sample(&s, bar(2.875)));
    // A split at the playhead's grid line.
    s.dispatch(Action::SelectClips {
        clips: vec![c],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(3.125),
        bar(3.125),
    ))))
    .unwrap();
    s.dispatch(Action::Separate).unwrap();
    let pieces = s.project().clips_of(locked).len();
    assert_eq!(pieces, 2);
    for piece in s.project().clips_of(locked) {
        assert!(on_sample(&s, piece.start), "{:?}", piece.start);
    }
    // Copy and paste at a grid position.
    let first = s.project().clips_of(locked)[0].id;
    s.dispatch(Action::SelectClips {
        clips: vec![first],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(None)).unwrap();
    s.dispatch(Action::Copy).unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: vec![locked],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Locate(bar(6.25))))
        .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(6.25),
        bar(6.25),
    ))))
    .unwrap();
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(s.project().clips_of(locked).len(), 3);
    for piece in s.project().clips_of(locked) {
        assert!(on_sample(&s, piece.start), "{:?}", piece.start);
    }
    // Duplicate: right after the original, on its sample.
    s.dispatch(Action::SelectClips {
        clips: vec![first],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SetEditRange(None)).unwrap();
    s.dispatch(Action::Repeat(1)).unwrap();
    assert_eq!(s.project().clips_of(locked).len(), 4);
    for piece in s.project().clips_of(locked) {
        assert!(on_sample(&s, piece.start), "{:?}", piece.start);
    }
    // All of it stays put through a tempo change.
    let at: Vec<i64> = s
        .project()
        .clips_of(locked)
        .iter()
        .map(|c| sample(&s, c.start))
        .collect();
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 151.0 }))
        .unwrap();
    let now: Vec<i64> = s
        .project()
        .clips_of(locked)
        .iter()
        .map(|c| sample(&s, c.start))
        .collect();
    assert_eq!(now, at);
}

/// Choosing the time base for one of several selected tracks sets it for
/// them all (one undo step); a duplicate keeps it.
#[test]
fn the_selected_tracks_take_the_time_base_and_duplicates_keep_it() {
    let mut s = session();
    let a = s.add_track(TrackKind::Audio).unwrap();
    let b = s.add_track(TrackKind::Instrument).unwrap();
    let c = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: vec![a, b],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let n = steps(&s);
    lock(&mut s, a);
    assert!(s.is_time_locked(a) && s.is_time_locked(b));
    assert!(!s.is_time_locked(c));
    assert_eq!(steps(&s), n + 1);
    let choices = s.time_base_choices(a);
    assert_eq!(
        choices
            .iter()
            .map(|c| (c.label.as_str(), c.checked))
            .collect::<Vec<_>>(),
        vec![("Musical", false), ("Time-locked", true)]
    );
    let had: Vec<TrackId> = s.project().tracks.iter().map(|t| t.id).collect();
    s.dispatch(Action::DuplicateTracks {
        tracks: vec![a],
        content: true,
    })
    .unwrap();
    let copy = s
        .project()
        .tracks
        .iter()
        .find(|t| !had.contains(&t.id))
        .unwrap();
    assert!(copy.is_time_locked(), "{}", copy.name);
    undo(&mut s);
    undo(&mut s);
    assert!(!s.is_time_locked(a) && !s.is_time_locked(b));
}

/// A take recorded onto a time-locked track lands on its sample and stays
/// there when the tempo changes (ADR takes are recorded the same way).
#[test]
fn a_take_on_a_time_locked_track_keeps_its_sample() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_project::InputRouting;
    use faderframe_session::AudioPreferences;
    use std::time::{Duration, Instant};

    let mut s = Session::new(Project::new("Take", SR), None, EngineConfig::default()).unwrap();
    s.edit(Command::SetTempo { bpm: 97.0 }).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    lock(&mut s, t);
    s.edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Hardware { first_channel: 0 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(440.0))],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    // From between two samples' worth of a grid line.
    let from = bar(0.3);
    s.dispatch(Action::Transport(TransportAction::Locate(from)))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let until = sample(&s, from) + SR as i64 / 3;
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.transport().position < until {
        assert!(Instant::now() < deadline, "timed out recording");
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(2));
    }
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let take = s.project().clips_of(t)[0].clone();
    let rate = f64::from(SR);
    let tl = &s.project().timeline;
    let at = tl.to_samples(take.start, rate);
    assert_eq!(tl.at_sample(at, rate), take.start, "on a whole sample");
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 133.0 }))
        .unwrap();
    assert_eq!(start(&s, take.id), at, "where it was recorded");
    s.stop_audio();
}
