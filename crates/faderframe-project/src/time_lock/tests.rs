#![allow(clippy::unwrap_used)]

use super::*;
use crate::ripple::RippleShift;
use crate::{
    AudioClip, Clip, ClipContent, ClipFades, Freeze, MidiClip, StretchSettings, TimeBase, Track,
    TrackColor, TrackKind,
};
use faderframe_automation::{AutomationMode, AutomationTarget, CurveShape};
use faderframe_core::{AudioSourceId, AutomationLaneId, ClipId, TrackId};
use faderframe_timeline::{TempoCurve, TempoPoint};

const SR: u32 = 48_000;

fn q(quarters: f64) -> MusicalTime {
    MusicalTime::from_quarters(quarters)
}

fn audio(id: u64, track: TrackId, start: MusicalTime) -> Clip {
    Clip {
        id: ClipId(id),
        track,
        name: "a".into(),
        color: None,
        start,
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source: AudioSourceId(1),
            source_offset: 0,
            length: 24_000,
            gain_db: 0.0,
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
        }),
    }
}

fn midi(id: u64, track: TrackId, start: MusicalTime) -> Clip {
    Clip {
        id: ClipId(id),
        track,
        name: "m".into(),
        color: None,
        start,
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: q(4.0),
            ..MidiClip::default()
        }),
    }
}

/// A locked audio track (a clip at 8.3 quarters, a volume lane), a locked
/// instrument track (a MIDI clip at 6 quarters) and a musical audio track
/// (a clip at 8.3 quarters), at 120 BPM.
fn project() -> (Project, [TrackId; 3]) {
    let mut p = Project::new("Lock", SR);
    let ids = [TrackId(101), TrackId(102), TrackId(103)];
    let kinds = [TrackKind::Audio, TrackKind::Instrument, TrackKind::Audio];
    for (id, kind) in ids.iter().zip(kinds) {
        p.tracks
            .push(Track::new(*id, kind, "t", TrackColor::palette(0)));
    }
    p.track_mut(ids[0]).unwrap().time_base = TimeBase::TimeLocked;
    p.track_mut(ids[1]).unwrap().time_base = TimeBase::TimeLocked;
    for c in [
        audio(10, ids[0], q(8.3)),
        midi(11, ids[1], q(6.0)),
        audio(12, ids[2], q(8.3)),
    ] {
        p.track_mut(c.track).unwrap().clips.push(c.id);
        p.clips.insert(c.id, c);
    }
    p.track_mut(ids[0])
        .unwrap()
        .automation
        .lanes
        .push(AutomationLane {
            id: AutomationLaneId(20),
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::from_points(vec![
                AutomationPoint {
                    time: q(2.0),
                    value: 0.0,
                    shape: CurveShape::Linear,
                },
                AutomationPoint {
                    time: q(12.5),
                    value: -6.0,
                    shape: CurveShape::Smooth,
                },
            ]),
            mode: AutomationMode::Read,
            visible: true,
        });
    (p, ids)
}

/// Apply `cmd` and the moves the time lock asks for.
fn edit(p: &mut Project, cmd: Command) {
    let lock = plan(p, &cmd);
    cmd.apply(p).unwrap();
    if let Some(lock) = lock {
        for c in lock.keep(p) {
            c.apply(p).unwrap();
        }
    }
}

fn sample(p: &Project, t: MusicalTime) -> i64 {
    p.timeline.to_samples(t, f64::from(SR))
}

fn slower(p: &Project) -> Command {
    let mut tl = p.timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: q(1.0),
        bpm: 87.0,
        curve: TempoCurve::Linear,
    });
    tl.tempo.set_point(TempoPoint {
        position: q(5.0),
        bpm: 131.0,
        curve: TempoCurve::Constant,
    });
    Command::SetTimeline {
        timeline: Box::new(tl),
    }
}

fn points(p: &Project) -> Vec<i64> {
    p.track(TrackId(101)).unwrap().automation.lanes[0]
        .curve
        .points()
        .iter()
        .map(|x| sample(p, x.time))
        .collect()
}

#[test]
fn a_tempo_change_keeps_locked_material_on_its_samples() {
    let (mut p, _) = project();
    let before: Vec<i64> = [10, 11, 12]
        .map(|c| sample(&p, p.clip(ClipId(c)).unwrap().start))
        .to_vec();
    let lane_before = points(&p);
    let cmd = slower(&p);
    edit(&mut p, cmd);
    let after: Vec<i64> = [10, 11, 12]
        .map(|c| sample(&p, p.clip(ClipId(c)).unwrap().start))
        .to_vec();
    assert_eq!(after[0], before[0], "the locked audio clip stays");
    assert_eq!(after[1], before[1], "the locked MIDI clip's start stays");
    assert_ne!(after[2], before[2], "the musical clip moves with the beats");
    assert_eq!(p.clip(ClipId(12)).unwrap().start, q(8.3));
    assert_eq!(
        points(&p),
        lane_before,
        "the locked track's automation stays"
    );
    // The MIDI clip's notes follow the tempo from its start: its length
    // stays in beats.
    assert_eq!(
        p.clip(ClipId(11)).unwrap().as_midi().unwrap().length,
        q(4.0)
    );
    // Many changes in a row (a tempo-lane drag) never drift.
    for bpm in [60.0, 173.5, 99.9, 120.0, 41.0] {
        let mut tl = p.timeline.clone();
        tl.tempo.set_initial_bpm(bpm);
        edit(
            &mut p,
            Command::SetTimeline {
                timeline: Box::new(tl),
            },
        );
        assert_eq!(sample(&p, p.clip(ClipId(10)).unwrap().start), before[0]);
        assert_eq!(sample(&p, p.clip(ClipId(11)).unwrap().start), before[1]);
        assert_eq!(points(&p), lane_before);
    }
}

#[test]
fn set_tempo_and_a_frozen_render_keep_their_place() {
    let (mut p, t) = project();
    p.track_mut(t[0]).unwrap().freeze = Some(Freeze {
        source: AudioSourceId(2),
        start: q(3.7),
        length: 48_000,
        latency: 0,
    });
    let clip = sample(&p, p.clip(ClipId(10)).unwrap().start);
    let render = sample(&p, q(3.7));
    edit(&mut p, Command::SetTempo { bpm: 77.7 });
    assert_eq!(sample(&p, p.clip(ClipId(10)).unwrap().start), clip);
    let f = p.track(t[0]).unwrap().freeze.clone().unwrap();
    assert_eq!(sample(&p, f.start), render);
}

#[test]
fn nothing_to_keep_without_locked_tracks_or_tempo_changes() {
    let (mut p, t) = project();
    // A plain edit on a musical track: nothing.
    assert!(
        plan(
            &p,
            &Command::RenameTrack {
                track: t[2],
                name: "x".into()
            }
        )
        .is_none()
    );
    assert!(
        plan(
            &p,
            &Command::MoveClip {
                clip: ClipId(12),
                track: t[2],
                start: q(1.0)
            }
        )
        .is_none()
    );
    for tr in &mut p.tracks {
        tr.time_base = TimeBase::Musical;
    }
    assert!(plan(&p, &slower(&p)).is_none());
}

#[test]
fn positions_given_on_a_locked_track_land_on_whole_samples() {
    let (mut p, t) = project();
    p.timeline.tempo.set_initial_bpm(97.0);
    // A grid position at 97 BPM is not a whole sample.
    let grid = q(5.25);
    let exact = p.timeline.tempo.musical_to_seconds(grid) * f64::from(SR);
    assert!(exact.fract().abs() > 1e-6, "{exact}");
    edit(
        &mut p,
        Command::MoveClip {
            clip: ClipId(10),
            track: t[0],
            start: grid,
        },
    );
    let start = p.clip(ClipId(10)).unwrap().start;
    assert_ne!(start, grid, "moved onto the sample");
    assert_eq!(
        sample(&p, start),
        sample(&p, grid),
        "the grid line's sample"
    );
    assert_eq!(
        p.timeline.at_sample(sample(&p, start), f64::from(SR)),
        start
    );
    // A musical track keeps the grid position as given.
    edit(
        &mut p,
        Command::MoveClip {
            clip: ClipId(12),
            track: t[2],
            start: grid,
        },
    );
    assert_eq!(p.clip(ClipId(12)).unwrap().start, grid);
}

/// Clip commands before a tempo change in a batch are read against the
/// tempo before it (a ripple's), those after it against the new one.
#[test]
fn positions_are_read_against_the_tempo_in_effect_where_given() {
    let (mut p, t) = project();
    let old = p.timeline.clone();
    let Command::SetTimeline { timeline } = slower(&p) else {
        unreachable!()
    };
    let new = (*timeline).clone();
    edit(
        &mut p,
        Command::Batch {
            label: "x".into(),
            commands: vec![
                Command::MoveClip {
                    clip: ClipId(10),
                    track: t[0],
                    start: q(10.0),
                },
                Command::SetTimeline {
                    timeline: Box::new(new.clone()),
                },
                Command::MoveClip {
                    clip: ClipId(11),
                    track: t[1],
                    start: q(10.0),
                },
            ],
        },
    );
    let rate = f64::from(SR);
    assert_eq!(
        sample(&p, p.clip(ClipId(10)).unwrap().start),
        old.to_samples(q(10.0), rate)
    );
    assert_eq!(
        sample(&p, p.clip(ClipId(11)).unwrap().start),
        new.to_samples(q(10.0), rate)
    );
}

#[test]
fn a_ripple_on_a_locked_track_moves_by_the_time_taken_out() {
    let (mut p, _) = project();
    let mut tl = p.timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: q(16.0),
        bpm: 60.0,
        curve: TempoCurve::Constant,
    });
    p.timeline = tl.clone();
    let rate = f64::from(SR);
    // Take out [4, 8) quarters (2 s at 120 BPM).
    let cut = RippleShift::remove(q(4.0), q(8.0));
    let taken = tl.to_samples(q(8.0), rate) - tl.to_samples(q(4.0), rate);
    assert_eq!(taken, 96_000);
    // Past the tempo change a quarter lasts a second: musically the
    // position moves by 4 seconds, in time by 2.
    let at = q(20.0);
    assert_eq!(cut.map(at), q(16.0));
    let moved = cut.map_in_time(at, &tl, rate);
    assert_eq!(tl.to_samples(moved, rate), tl.to_samples(at, rate) - taken);
    // Before the cut nothing moves; inside the span it lands on its start.
    assert_eq!(cut.map_in_time(q(1.3), &tl, rate), q(1.3));
    assert_eq!(
        tl.to_samples(cut.map_in_time(q(6.0), &tl, rate), rate),
        tl.to_samples(q(4.0), rate)
    );
    // The automation the same way.
    let mut curve = AutomationCurve::from_points(vec![
        AutomationPoint {
            time: q(0.0),
            value: 1.0,
            shape: CurveShape::Linear,
        },
        AutomationPoint {
            time: q(20.0),
            value: 2.0,
            shape: CurveShape::Linear,
        },
    ]);
    ripple_curve_in_time(&mut curve, &[cut], &tl, rate);
    let last = curve.points().last().unwrap();
    assert_eq!(
        tl.to_samples(last.time, rate),
        tl.to_samples(at, rate) - taken
    );
    assert_eq!(last.value, 2.0);
}

#[test]
fn the_time_base_is_saved_and_old_files_are_musical() {
    let (p, t) = project();
    let text = crate::file::to_string(&p, None).unwrap();
    let back = crate::file::from_str(&text).unwrap().project;
    assert_eq!(back.track(t[0]).unwrap().time_base, TimeBase::TimeLocked);
    assert_eq!(back.track(t[2]).unwrap().time_base, TimeBase::Musical);
    // Musical tracks write nothing: files from before read as musical.
    let musical = serde_json::to_value(p.track(t[2]).unwrap()).unwrap();
    assert!(musical.get("time_base").is_none());
    let locked = serde_json::to_value(p.track(t[0]).unwrap()).unwrap();
    assert_eq!(locked["time_base"], "time_locked");
}

#[test]
fn setting_the_time_base_moves_nothing_and_undoes() {
    let (mut p, t) = project();
    let before = p.clone();
    let cmd = Command::SetTrackTimeBase {
        track: t[2],
        base: TimeBase::TimeLocked,
    };
    assert_eq!(cmd.impact(), crate::Impact::None);
    assert_eq!(cmd.label(), "Time Base: Time-locked");
    let inverse = cmd.apply(&mut p).unwrap();
    assert!(p.track(t[2]).unwrap().is_time_locked());
    assert_eq!(p.clips, before.clips);
    inverse.apply(&mut p).unwrap();
    assert_eq!(p, before);
}

/// ADR cues belong to the picture: a tempo change leaves them on their
/// samples even in a project without time-locked tracks, and cues set in
/// the same edit after the tempo change are taken as given.
#[test]
fn adr_cues_stay_with_the_picture() {
    use crate::adr::{Adr, AdrCue};
    use faderframe_core::AdrCueId;
    let mut p = Project::new("ADR", SR);
    let cue = |id: u64, start: f64, end: f64| AdrCue {
        id: AdrCueId(id),
        number: id.to_string(),
        character: String::new(),
        text: "a line".into(),
        start: q(start),
        end: q(end),
        track: None,
        note: String::new(),
        done: false,
    };
    p.adr = Adr {
        cues: vec![cue(1, 6.3, 9.1), cue(2, 14.0, 17.75)],
        ..Adr::default()
    };
    let before: Vec<(i64, i64)> = p
        .adr
        .cues
        .iter()
        .map(|c| (sample(&p, c.start), sample(&p, c.end)))
        .collect();
    let slow = slower(&p);
    edit(&mut p, slow);
    let after: Vec<(i64, i64)> = p
        .adr
        .cues
        .iter()
        .map(|c| (sample(&p, c.start), sample(&p, c.end)))
        .collect();
    assert_eq!(after, before, "the cues keep their samples");
    assert_ne!(p.adr.cues[0].start, q(6.3), "their beats moved instead");
    // A new cue list given after a tempo change in one batch: as given.
    let mut faster = p.timeline.clone();
    faster.tempo.set_initial_bpm(150.0);
    let mut given = p.adr.clone();
    given.cues[1].start = q(20.0);
    edit(
        &mut p,
        Command::Batch {
            label: "both".into(),
            commands: vec![
                Command::SetTimeline {
                    timeline: Box::new(faster),
                },
                Command::SetAdr {
                    adr: Box::new(given),
                },
            ],
        },
    );
    assert_eq!(p.adr.cues[1].start, q(20.0));
    // Nothing to keep without cues or locked tracks.
    let mut bare = Project::new("Bare", SR);
    let slow = slower(&bare);
    assert!(plan(&bare, &slow).is_none());
    bare.adr.cues.push(cue(3, 1.0, 2.0));
    assert!(plan(&bare, &slow).is_some());
}
