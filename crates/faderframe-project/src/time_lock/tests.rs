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
            envelope: None,
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

// --- Material placed in time on its own: locked notes, markers, sections.

fn note(id: u64, start: f64, length: f64, locked: bool) -> crate::MidiNote {
    crate::MidiNote {
        articulation: None,
        id: faderframe_core::NoteId(id),
        start: q(start),
        length: q(length),
        key: 60,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: locked,
        fine: None,
    }
}

/// A musical instrument track holding a cue at 8 quarters: a locked hit
/// at 2.37 quarters into it (with a pitch curve), a plain note at 1, a
/// controller point at 3, at 120 BPM.
fn cue_project() -> (Project, TrackId, ClipId) {
    let (mut p, _) = project();
    let t = TrackId(104);
    p.tracks.push(Track::new(
        t,
        TrackKind::Instrument,
        "cue",
        TrackColor::palette(1),
    ));
    let mut m = MidiClip {
        length: q(4.0),
        notes: vec![note(1, 1.0, 0.5, false), note(2, 2.37, 0.6, true)],
        ..MidiClip::default()
    };
    let e = m.expression_mut(faderframe_core::NoteId(2));
    e.pitch = vec![
        crate::ExpressionPoint {
            time: q(0.0),
            value: 0.0,
        },
        crate::ExpressionPoint {
            time: q(0.4),
            value: 1.5,
        },
    ];
    m.lane_mut(crate::MidiController::Cc { number: 1 }, 0)
        .points
        .push(crate::ControllerPoint {
            time: q(3.0),
            value: 64,
            fine: None,
        });
    let c = Clip {
        content: ClipContent::Midi(m),
        ..midi(30, t, q(8.0))
    };
    p.track_mut(t).unwrap().clips.push(c.id);
    p.clips.insert(c.id, c);
    (p, t, ClipId(30))
}

fn cue(p: &Project, c: ClipId) -> (MusicalTime, MidiClip) {
    let clip = p.clip(c).unwrap();
    (clip.start, clip.as_midi().unwrap().clone())
}

/// Where a note of a clip sounds: (start, end) samples.
fn heard(p: &Project, c: ClipId, n: u64) -> (i64, i64) {
    let (start, m) = cue(p, c);
    let n = m.note(faderframe_core::NoteId(n)).unwrap();
    (sample(p, start + n.start), sample(p, start + n.end()))
}

/// The samples of a note's pitch curve points.
fn curve_heard(p: &Project, c: ClipId, n: u64) -> Vec<i64> {
    let (start, m) = cue(p, c);
    let note = m.note(faderframe_core::NoteId(n)).unwrap();
    m.expression(note.id)
        .unwrap()
        .pitch
        .iter()
        .map(|x| sample(p, start + note.start + x.time))
        .collect()
}

#[test]
fn a_locked_note_keeps_its_samples_while_the_others_follow_the_beats() {
    let (mut p, _, c) = cue_project();
    let hit = heard(&p, c, 2);
    let curve = curve_heard(&p, c, 2);
    let plain = heard(&p, c, 1);
    let cmd = slower(&p);
    edit(&mut p, cmd);
    let (start, m) = cue(&p, c);
    assert_eq!(start, q(8.0), "the clip follows the beats");
    assert_eq!(m.note(faderframe_core::NoteId(1)).unwrap().start, q(1.0));
    assert_ne!(heard(&p, c, 1), plain, "so does the plain note");
    assert_eq!(heard(&p, c, 2), hit, "the hit stays where it sounds");
    assert_eq!(curve_heard(&p, c, 2), curve, "its pitch curve too");
    assert_ne!(m.note(faderframe_core::NoteId(2)).unwrap().start, q(2.37));
    // A tempo-lane drag (many changes) never drifts.
    for bpm in [60.0, 173.5, 99.9, 120.0, 41.0, 133.0] {
        let mut tl = p.timeline.clone();
        tl.tempo.set_initial_bpm(bpm);
        edit(
            &mut p,
            Command::SetTimeline {
                timeline: Box::new(tl),
            },
        );
        assert_eq!(heard(&p, c, 2), hit, "{bpm}");
        assert_eq!(curve_heard(&p, c, 2), curve, "{bpm}");
    }
}

#[test]
fn a_locked_note_in_a_clip_on_a_locked_track_keeps_its_samples_too() {
    let (mut p, t) = project();
    let c = ClipId(11);
    if let Some(m) = p.clips.get_mut(&c).and_then(|c| c.as_midi_mut()) {
        m.notes = vec![note(1, 0.5, 1.0, false), note(2, 1.75, 0.25, true)];
    }
    let clip = sample(&p, p.clip(c).unwrap().start);
    let hit = heard(&p, c, 2);
    let plain = heard(&p, c, 1);
    let cmd = slower(&p);
    edit(&mut p, cmd);
    assert_eq!(sample(&p, p.clip(c).unwrap().start), clip, "the clip stays");
    assert_eq!(heard(&p, c, 2), hit, "the locked note stays");
    assert_ne!(
        heard(&p, c, 1),
        plain,
        "the plain note plays at the new tempo"
    );
    assert_eq!(p.clip(c).unwrap().track, t[1]);
}

/// A tempo change that would take a locked note out of its clip: the
/// clip grows to hold it, the rest keeps its place in the music.
#[test]
fn a_clip_grows_to_hold_its_locked_notes() {
    let (mut p, _, c) = cue_project();
    // The hit at the clip's very start; another hit near its end.
    if let Some(m) = p.clips.get_mut(&c).and_then(|c| c.as_midi_mut()) {
        m.notes[1].start = MusicalTime::ZERO;
        m.notes.push(note(3, 3.6, 0.3, true));
        m.sort_notes();
    }
    let early = heard(&p, c, 2);
    let late = heard(&p, c, 3);
    // Slower before the clip: it starts later in time, the hit stays.
    let mut tl = p.timeline.clone();
    tl.tempo.set_initial_bpm(100.0);
    edit(
        &mut p,
        Command::SetTimeline {
            timeline: Box::new(tl),
        },
    );
    let (start, m) = cue(&p, c);
    assert_eq!(heard(&p, c, 2), early);
    assert!(start < q(8.0), "grew back to the hit");
    assert_eq!(
        start + m.note(faderframe_core::NoteId(2)).unwrap().start,
        start
    );
    // What it holds is where it was in the music.
    let plain = m.note(faderframe_core::NoteId(1)).unwrap();
    assert_eq!(start + plain.start, q(9.0));
    let cc = m.controllers[0].points[0];
    assert_eq!(start + cc.time, q(11.0));
    assert_eq!(start + m.length, q(12.0));
    // Faster inside it: its end comes earlier, the late hit stays and
    // the clip grows to hold it.
    let mut tl = p.timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: q(7.0),
        bpm: 240.0,
        curve: TempoCurve::Constant,
    });
    edit(
        &mut p,
        Command::SetTimeline {
            timeline: Box::new(tl),
        },
    );
    let (start, m) = cue(&p, c);
    assert_eq!(heard(&p, c, 3), late);
    let n3 = m.note(faderframe_core::NoteId(3)).unwrap();
    assert!(n3.end() <= m.length, "still in the clip");
    assert!(start + m.length > q(12.0));
}

#[test]
fn a_moved_clip_carries_its_locked_notes_at_the_same_time_from_its_start() {
    let (mut p, t, c) = cue_project();
    let mut tl = p.timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: q(16.0),
        bpm: 77.0,
        curve: TempoCurve::Constant,
    });
    p.timeline = tl;
    let offset = |p: &Project| heard(p, c, 2).0 - sample(p, p.clip(c).unwrap().start);
    let before = offset(&p);
    edit(
        &mut p,
        Command::MoveClip {
            clip: c,
            track: t,
            start: q(20.0),
        },
    );
    assert_eq!(p.clip(c).unwrap().start, q(20.0));
    assert_eq!(offset(&p), before, "the hit as long after the start");
    let (_, m) = cue(&p, c);
    assert_eq!(
        m.note(faderframe_core::NoteId(1)).unwrap().start,
        q(1.0),
        "the plain note keeps its beat"
    );
    // And back: on the very sample it had.
    let hit = heard(&p, c, 2);
    edit(
        &mut p,
        Command::MoveClip {
            clip: c,
            track: t,
            start: q(30.0),
        },
    );
    edit(
        &mut p,
        Command::MoveClip {
            clip: c,
            track: t,
            start: q(20.0),
        },
    );
    assert_eq!(heard(&p, c, 2), hit);
}

#[test]
fn locking_a_note_puts_it_on_a_whole_sample() {
    let (mut p, _, c) = cue_project();
    p.timeline.tempo.set_initial_bpm(97.0);
    let (start, m) = cue(&p, c);
    let mut n = *m.note(faderframe_core::NoteId(1)).unwrap();
    let exact = p.timeline.tempo.musical_to_seconds(start + n.start) * f64::from(SR);
    assert!(exact.fract().abs() > 1e-6, "between samples: {exact}");
    let grid = sample(&p, start + n.start);
    n.time_locked = true;
    edit(&mut p, Command::UpdateNote { clip: c, note: n });
    let (start, m) = cue(&p, c);
    let n = m.note(faderframe_core::NoteId(1)).unwrap();
    assert!(n.time_locked);
    assert_eq!(sample(&p, start + n.start), grid, "the grid's sample");
    assert_eq!(
        p.timeline.at_sample(grid, f64::from(SR)),
        start + n.start,
        "exactly on it"
    );
    // An edit that leaves the locked notes alone changes none of them.
    let lock = plan(
        &p,
        &Command::RenameClip {
            clip: c,
            name: "x".into(),
        },
    );
    assert!(lock.is_none_or(|l| l.keep(&p).is_empty()));
}

fn marker(id: u64, at: f64, locked: bool) -> crate::Marker {
    crate::Marker {
        id: faderframe_core::MarkerId(id),
        position: q(at),
        name: format!("M{id}"),
        time_base: if locked {
            TimeBase::TimeLocked
        } else {
            TimeBase::Musical
        },
    }
}

fn section(id: u64, a: f64, b: f64, locked: bool) -> crate::Section {
    crate::Section {
        id: faderframe_core::SectionId(id),
        name: format!("S{id}"),
        start: q(a),
        end: q(b),
        color: TrackColor::palette(0),
        time_base: if locked {
            TimeBase::TimeLocked
        } else {
            TimeBase::Musical
        },
    }
}

#[test]
fn locked_markers_and_sections_keep_their_samples() {
    // No locked track at all: the markers alone ask for it.
    let mut p = Project::new("Markers", SR);
    p.markers = vec![marker(1, 6.0, true), marker(2, 6.0, false)];
    p.sections = vec![section(3, 4.0, 10.5, true), section(4, 10.5, 16.0, false)];
    let at = |p: &Project, i: usize| sample(p, p.markers[i].position);
    let span = |p: &Project, i: usize| {
        let s = &p.sections[i];
        (sample(p, s.start), sample(p, s.end))
    };
    let (locked, plain) = (at(&p, 0), at(&p, 1));
    let (scene, verse) = (span(&p, 0), span(&p, 1));
    let cmd = slower(&p);
    edit(&mut p, cmd);
    assert_eq!(at(&p, 0), locked, "the locked marker stays");
    assert_ne!(at(&p, 1), plain, "the musical one moves");
    assert_eq!(p.markers[1].position, q(6.0));
    assert_eq!(span(&p, 0), scene, "the locked section stays");
    assert_ne!(span(&p, 1), verse);
    for bpm in [61.0, 177.7, 120.0] {
        let mut tl = p.timeline.clone();
        tl.tempo.set_initial_bpm(bpm);
        edit(
            &mut p,
            Command::SetTimeline {
                timeline: Box::new(tl),
            },
        );
        assert_eq!(at(&p, 0), locked, "{bpm}");
        assert_eq!(span(&p, 0), scene, "{bpm}");
    }
    // A marker locked (or added locked) lands on a whole sample.
    p.timeline.tempo = TempoMap::new(97.0);
    let grid = q(5.25);
    edit(
        &mut p,
        Command::UpdateMarker {
            marker: crate::Marker {
                position: grid,
                ..marker(2, 0.0, true)
            },
        },
    );
    let m = p
        .markers
        .iter()
        .find(|m| m.id == faderframe_core::MarkerId(2))
        .unwrap();
    assert!(m.is_time_locked());
    assert_ne!(m.position, grid);
    assert_eq!(sample(&p, m.position), sample(&p, grid));
    assert_eq!(
        p.timeline.at_sample(sample(&p, grid), f64::from(SR)),
        m.position
    );
}

#[test]
fn ripples_move_locked_markers_and_sections_by_the_time_taken_out() {
    let mut p = Project::new("Ripple", SR);
    let mut tl = p.timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: q(12.0),
        bpm: 60.0,
        curve: TempoCurve::Constant,
    });
    p.timeline = tl.clone();
    p.markers = vec![marker(1, 20.0, true), marker(2, 20.0, false)];
    p.sections = vec![section(3, 16.0, 24.0, true)];
    let rate = f64::from(SR);
    let cut = RippleShift::remove(q(4.0), q(8.0));
    let taken = tl.to_samples(q(8.0), rate) - tl.to_samples(q(4.0), rate);
    let (locked, scene) = (
        sample(&p, q(20.0)),
        (sample(&p, q(16.0)), sample(&p, q(24.0))),
    );
    let cmds = crate::ripple::global_commands(&p, &[cut], crate::ripple::RippleGlobals::ALL);
    edit(
        &mut p,
        Command::Batch {
            label: "Ripple".into(),
            commands: cmds,
        },
    );
    assert_eq!(sample(&p, p.markers[0].position), locked - taken);
    let s = &p.sections[0];
    assert_eq!(
        (sample(&p, s.start), sample(&p, s.end)),
        (scene.0 - taken, scene.1 - taken)
    );
    // The musical marker moved by its beats (with the tempo along, they
    // last as long as before).
    assert_eq!(p.markers[1].position, q(16.0));
}

#[test]
fn locks_are_saved_and_old_files_have_none() {
    let (mut p, _, c) = cue_project();
    p.markers = vec![marker(1, 2.0, true), marker(2, 3.0, false)];
    p.sections = vec![section(3, 0.0, 4.0, true)];
    let text = crate::file::to_string(&p, None).unwrap();
    let back = crate::file::from_str(&text).unwrap().project;
    assert_eq!(back.markers, p.markers);
    assert_eq!(back.sections, p.sections);
    assert_eq!(cue(&back, c).1, cue(&p, c).1);
    // Unlocked ones write nothing: older files read as musical.
    let plain = serde_json::to_value(&p.markers[1]).unwrap();
    assert!(plain.get("time_base").is_none());
    let (_, m) = cue(&p, c);
    let n = serde_json::to_value(m.note(faderframe_core::NoteId(1)).unwrap()).unwrap();
    assert!(n.get("time_locked").is_none());
    let n = serde_json::to_value(m.note(faderframe_core::NoteId(2)).unwrap()).unwrap();
    assert_eq!(n["time_locked"], true);
}
