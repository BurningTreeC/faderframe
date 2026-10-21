//! The arranger's clipboard: copy, cut, paste, duplicate and repeat of
//! clips and ranges across tracks.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_automation::{AutomationCurve, AutomationLane, AutomationPoint, AutomationTarget};
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, ControllerLane, ControllerPoint,
    ExpressionPoint, MidiClip, MidiController, MidiNote, NoteExpression, Project, SourceSpec,
    StretchSettings, SysexEvent, TrackKind, Warp,
};
use faderframe_session::clipboard::ClipboardKind;
use faderframe_session::{Action, EditMode, EditRange, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// Frames per bar at 120 BPM in 4/4.
const BAR: i64 = 2 * SR as i64;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

struct Song {
    s: Session,
    /// Audio tracks A, B, C, an instrument track M, an audio track D.
    a: TrackId,
    b: TrackId,
    c: TrackId,
    m: TrackId,
    d: TrackId,
    source: AudioSourceId,
}

fn song() -> Song {
    let mut s = Session::new(Project::new("Clipboard", SR), None, EngineConfig::default()).unwrap();
    let a = s.add_track(TrackKind::Audio).unwrap();
    let b = s.add_track(TrackKind::Audio).unwrap();
    let c = s.add_track(TrackKind::Audio).unwrap();
    let m = s.add_track(TrackKind::Instrument).unwrap();
    let d = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(9000);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "loop".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 32,
                    seed: 1,
                },
            },
        }),
    }))
    .unwrap();
    Song {
        s,
        a,
        b,
        c,
        m,
        d,
        source,
    }
}

fn audio_clip(t: TrackId, source: AudioSourceId, start: f64, bars: f64, id: u64) -> Clip {
    Clip {
        id: ClipId(id),
        track: t,
        name: format!("clip {id}"),
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
    }
}

fn add(s: &mut Session, clip: Clip) -> ClipId {
    let id = clip.id;
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(clip),
    }))
    .unwrap();
    id
}

fn midi_clip(t: TrackId, start: f64, id: u64) -> Clip {
    let q = MusicalTime::from_quarters;
    let note = |n: u64, at: f64, key: u8| MidiNote {
        id: NoteId(n),
        start: q(at),
        length: q(0.5),
        key,
        velocity: 90,
        channel: 0,
        muted: false,
        release: Some(40),
    };
    let mut wheel = ControllerLane::new(MidiController::Cc { number: 1 }, 0);
    wheel.points = vec![
        ControllerPoint {
            time: q(0.0),
            value: 5,
        },
        ControllerPoint {
            time: q(2.0),
            value: 100,
        },
    ];
    Clip {
        id: ClipId(id),
        track: t,
        name: "keys".into(),
        color: None,
        start: bar(start),
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: bar(1.0),
            notes: vec![note(id * 10 + 1, 0.0, 60), note(id * 10 + 2, 1.0, 64)],
            controllers: vec![wheel],
            expressions: vec![NoteExpression {
                pitch: vec![ExpressionPoint {
                    time: q(0.25),
                    value: 1.0,
                }],
                ..NoteExpression::new(NoteId(id * 10 + 2))
            }],
            sysex: vec![SysexEvent {
                time: q(0.5),
                data: vec![0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7],
            }],
        }),
    }
}

/// (start in bars, track) of a track's clips.
fn starts(s: &Session, t: TrackId) -> Vec<f64> {
    s.project()
        .clips_of(t)
        .iter()
        .map(|c| (c.start.quarters() / 4.0 * 1000.0).round() / 1000.0)
        .collect()
}

/// (start, end) in bars of a track's clips.
fn layout(s: &Session, t: TrackId) -> Vec<(f64, f64)> {
    let p = s.project();
    let r = |v: f64| (v * 1000.0).round() / 1000.0;
    p.clips_of(t)
        .iter()
        .map(|c| {
            (
                r(c.start.quarters() / 4.0),
                r(c.end(&p.timeline, p.sample_rate).quarters() / 4.0),
            )
        })
        .collect()
}

fn select(s: &mut Session, clips: &[ClipId], tracks: &[TrackId]) {
    s.dispatch(Action::SelectClips {
        clips: clips.to_vec(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: tracks.to_vec(),
        mode: SelectMode::Replace,
    })
    .unwrap();
}

fn locate(s: &mut Session, at: MusicalTime) {
    s.dispatch(Action::Transport(TransportAction::Locate(at)))
        .unwrap();
}

/// The content of a clip with its notes' ids taken out (pastes give
/// notes fresh ids).
fn content_shape(c: &Clip) -> ClipContent {
    let mut content = c.content.clone();
    if let ClipContent::Midi(m) = &mut content {
        let ids: Vec<NoteId> = m.notes.iter().map(|n| n.id).collect();
        for (i, n) in m.notes.iter_mut().enumerate() {
            n.id = NoteId(i as u64);
        }
        for e in &mut m.expressions {
            e.note = NoteId(ids.iter().position(|n| *n == e.note).unwrap() as u64);
        }
    }
    content
}

#[test]
fn copied_clips_paste_at_the_playhead_with_their_offsets_tracks_and_content() {
    let Song {
        mut s,
        a,
        b,
        c,
        m,
        source,
        ..
    } = song();
    // A warped, faded, quieter clip on A; a clip half a bar later on C
    // (nothing on B between them); a MIDI clip a bar later on M.
    let mut first = audio_clip(a, source, 1.0, 2.0, 100);
    if let ClipContent::Audio(au) = &mut first.content {
        au.gain_db = -4.5;
        au.fades.fade_in = 4_800;
        au.fades.fade_out = 9_600;
        au.warp = Some(Warp::uniform(au.length * 3 / 4));
    }
    let x = add(&mut s, first);
    let y = add(&mut s, audio_clip(c, source, 1.5, 1.0, 101));
    let z = add(&mut s, midi_clip(m, 2.0, 102));
    select(&mut s, &[x, y, z], &[a, c, m]);
    s.dispatch(Action::Copy).unwrap();
    assert_eq!(s.clipboard().kind(), ClipboardKind::Clips);
    assert_eq!(s.clipboard().clip_count(), 3);
    // Changing the original afterwards does not change what was copied.
    s.dispatch(Action::SetClipsGain {
        clips: vec![x],
        db: 6.0,
    })
    .unwrap();
    let originals: Vec<Clip> = [x, y, z]
        .iter()
        .map(|c| s.project().clip(*c).unwrap().clone())
        .collect();
    locate(&mut s, bar(8.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(s.history().undo_label(), Some("Paste"));
    assert_eq!(starts(&s, a), vec![1.0, 8.0]);
    assert!(
        starts(&s, b).is_empty(),
        "the track between keeps its place"
    );
    assert_eq!(starts(&s, c), vec![1.5, 8.5]);
    assert_eq!(starts(&s, m), vec![2.0, 9.0]);
    // The originals are untouched; the copies are new clips with all of
    // the content as it was copied.
    for o in &originals {
        assert_eq!(s.project().clip(o.id), Some(o));
    }
    let pasted: Vec<ClipId> = s.selection.clips.iter().copied().collect();
    assert_eq!(pasted.len(), 3, "the pasted clips are selected");
    for id in &pasted {
        assert!(![x, y, z].contains(id));
        assert!(!s.project().clip_links.contains_key(id), "never an alias");
    }
    let copy_of = |s: &Session, t: TrackId| {
        s.project()
            .clips_of(t)
            .into_iter()
            .find(|c| c.start >= bar(8.0))
            .cloned()
            .unwrap()
    };
    let pa = copy_of(&s, a);
    let ClipContent::Audio(au) = &pa.content else {
        panic!()
    };
    assert_eq!(au.gain_db, -4.5, "as copied, not as changed since");
    assert_eq!((au.fades.fade_in, au.fades.fade_out), (4_800, 9_600));
    assert_eq!(au.source_offset, BAR);
    assert!(au.warp.is_some());
    assert_eq!(pa.name, "clip 100");
    let pm = copy_of(&s, m);
    assert_eq!(content_shape(&pm), content_shape(&originals[2]));
    // Fresh note ids, the expression goes with its note.
    let pmidi = pm.as_midi().unwrap();
    let omidi = originals[2].as_midi().unwrap();
    assert!(pmidi.notes.iter().all(|n| omidi.note(n.id).is_none()));
    assert_eq!(pmidi.expressions[0].note, pmidi.notes[1].id);
    // Every track lists its clips.
    for t in [a, c, m] {
        let listed = &s.project().track(t).unwrap().clips;
        assert!(
            s.project()
                .clips_of(t)
                .iter()
                .all(|c| listed.contains(&c.id))
        );
    }
    // The insertion point moved to the end: pasting again goes after.
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(starts(&s, a), vec![1.0, 8.0, 10.0]);
    assert_eq!(starts(&s, c), vec![1.5, 8.5, 10.5]);
    assert_eq!(starts(&s, m), vec![2.0, 9.0, 11.0]);
    // One undo step each.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(starts(&s, a), vec![1.0, 8.0]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(starts(&s, a), vec![1.0]);
    assert_eq!(starts(&s, m), vec![2.0]);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(starts(&s, c), vec![1.5, 8.5]);
}

#[test]
fn the_destination_track_takes_the_top_lane_and_others_follow_where_they_fit() {
    let Song {
        mut s,
        a,
        b,
        c,
        m,
        d,
        source,
    } = song();
    let x = add(&mut s, audio_clip(a, source, 0.0, 1.0, 100));
    let y = add(&mut s, audio_clip(c, source, 0.0, 1.0, 101));
    select(&mut s, &[x, y], &[a, c]);
    s.dispatch(Action::Copy).unwrap();
    // B selected: A's clip goes there; C's (two rows further down) would
    // land on M, an instrument track, so it goes on down to D.
    select(&mut s, &[], &[b]);
    locate(&mut s, bar(4.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(starts(&s, b), vec![4.0]);
    assert!(
        starts(&s, m).is_empty(),
        "never audio on an instrument track"
    );
    assert_eq!(starts(&s, d), vec![4.0]);
    assert_eq!(
        s.selection.tracks.iter().copied().collect::<Vec<_>>(),
        vec![b, d]
    );
    // MIDI copied with only audio tracks below: nothing is pasted, a
    // notice says so.
    let z = add(&mut s, midi_clip(m, 0.0, 102));
    select(&mut s, &[z], &[m]);
    s.dispatch(Action::Copy).unwrap();
    select(&mut s, &[], &[d]);
    let clips = s.project().clips.len();
    let steps = s.history().undo_labels().len();
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(s.project().clips.len(), clips);
    assert_eq!(s.history().undo_labels().len(), steps, "no empty step");
    assert!(
        s.latest_notice()
            .is_some_and(|n| n.text.contains("not pasted"))
    );
    // With the copied track itself selected (as after copying), the
    // paste goes back to the copied tracks.
    select(&mut s, &[x, y], &[a, c]);
    s.dispatch(Action::Copy).unwrap();
    locate(&mut s, bar(12.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(starts(&s, a), vec![0.0, 12.0]);
    assert_eq!(starts(&s, c), vec![0.0, 12.0]);
    // Nothing selected: the copied tracks too.
    select(&mut s, &[], &[]);
    locate(&mut s, bar(14.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(starts(&s, a), vec![0.0, 12.0, 14.0]);
}

#[test]
fn a_paste_replaces_what_lies_under_it_and_shuffle_inserts() {
    let Song {
        mut s,
        a,
        b,
        source,
        ..
    } = song();
    let x = add(&mut s, audio_clip(a, source, 0.0, 1.0, 100));
    add(&mut s, audio_clip(b, source, 8.0, 8.0, 101));
    select(&mut s, &[x], &[a]);
    s.dispatch(Action::Copy).unwrap();
    select(&mut s, &[], &[b]);
    locate(&mut s, bar(10.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(layout(&s, b), vec![(8.0, 10.0), (10.0, 11.0), (11.0, 16.0)]);
    // The audio after the pasted clip plays as before.
    let right = s
        .project()
        .clips_of(b)
        .into_iter()
        .find(|c| c.start == bar(11.0))
        .cloned()
        .unwrap();
    assert_eq!(right.as_audio().unwrap().source_offset, 11 * BAR);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(layout(&s, b), vec![(8.0, 16.0)]);
    // Shuffle: the paste pushes what follows later.
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    select(&mut s, &[], &[b]);
    locate(&mut s, bar(10.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(layout(&s, b), vec![(8.0, 10.0), (10.0, 11.0), (11.0, 17.0)]);
}

#[test]
fn the_paste_point_follows_the_grid_and_relative_grid_keeps_the_offset() {
    let Song {
        mut s, a, source, ..
    } = song();
    // A clip starting a quarter into bar 2.
    let mut c = audio_clip(a, source, 2.0, 1.0, 100);
    c.start = bar(2.0) + MusicalTime::from_quarters(1.0);
    let x = add(&mut s, c);
    select(&mut s, &[x], &[a]);
    s.dispatch(Action::Copy).unwrap();
    // Grid (beats): the playhead a little after beat 1 of bar 6.
    s.dispatch(Action::SetGrid(faderframe_timeline::GridDivision::Bar))
        .unwrap();
    let off_grid = bar(6.0) + MusicalTime::from_quarters(0.3);
    s.dispatch(Action::SetEditFlag(
        faderframe_session::EditFlag::LinkTimeline,
        true,
    ))
    .unwrap();
    locate(&mut s, off_grid);
    s.dispatch(Action::Paste).unwrap();
    let pasted = *s.selection.clips.iter().next().unwrap();
    assert_eq!(s.project().clip(pasted).unwrap().start, bar(6.0));
    s.dispatch(Action::Undo).unwrap();
    // Relative grid: a quarter past the bar, as copied.
    s.dispatch(Action::SetGridMode(faderframe_session::GridMode::Relative))
        .unwrap();
    locate(&mut s, off_grid);
    s.dispatch(Action::Paste).unwrap();
    let pasted = *s.selection.clips.iter().next().unwrap();
    assert_eq!(
        s.project().clip(pasted).unwrap().start,
        bar(6.0) + MusicalTime::from_quarters(1.0)
    );
    s.dispatch(Action::Undo).unwrap();
    // Slip: exactly at the playhead.
    s.dispatch(Action::SetEditMode(EditMode::Slip)).unwrap();
    locate(&mut s, off_grid);
    s.dispatch(Action::Paste).unwrap();
    let pasted = *s.selection.clips.iter().next().unwrap();
    let start = s.project().clip(pasted).unwrap().start;
    assert!((start.quarters() - off_grid.quarters()).abs() < 1e-3);
}

#[test]
fn a_cut_clip_pastes_after_its_track_is_gone_and_its_import_undone() {
    let Song {
        mut s,
        a,
        b,
        source,
        ..
    } = song();
    let x = add(&mut s, audio_clip(a, source, 0.0, 1.0, 100));
    select(&mut s, &[x], &[a]);
    s.dispatch(Action::Cut).unwrap();
    assert!(s.project().clip(x).is_none());
    assert_eq!(s.history().undo_label(), Some("Cut Clip"));
    // The track goes, then the source (as undoing its import would).
    s.dispatch(Action::Edit(Command::RemoveTrack { track: a }))
        .unwrap();
    s.dispatch(Action::Edit(Command::RemoveSource { source }))
        .unwrap();
    assert!(!s.project().sources.contains_key(&source));
    select(&mut s, &[], &[b]);
    locate(&mut s, bar(3.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(starts(&s, b), vec![3.0]);
    assert!(
        s.project().sources.contains_key(&source),
        "registered again"
    );
    let pasted = s.project().clips_of(b)[0].clone();
    assert_eq!(pasted.as_audio().unwrap().source, source);
    // One step: undone, the source goes again.
    s.dispatch(Action::Undo).unwrap();
    assert!(starts(&s, b).is_empty());
    assert!(!s.project().sources.contains_key(&source));
    // The insertion point is not part of the undo history: it stays after
    // the undone paste. Pasting again works the same (the clipboard is
    // as it was).
    assert_eq!(s.playhead(), bar(4.0));
    locate(&mut s, bar(3.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(starts(&s, b), vec![3.0]);
}

#[test]
fn clips_whose_media_is_gone_are_left_out_with_a_notice() {
    let Song { mut s, a, b, .. } = song();
    let gone = AudioSourceId(9100);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: gone,
            name: "deleted".into(),
            spec: SourceSpec::File {
                path: std::env::temp_dir().join("faderframe-no-such-file-9100.wav"),
                channels: 2,
                frames: 10 * BAR,
                sample_rate: SR,
            },
        }),
    }))
    .unwrap();
    let x = add(&mut s, audio_clip(a, gone, 0.0, 1.0, 100));
    select(&mut s, &[x], &[a]);
    s.dispatch(Action::Copy).unwrap();
    s.dispatch(Action::Edit(Command::RemoveClip { clip: x }))
        .unwrap();
    s.dispatch(Action::Edit(Command::RemoveSource { source: gone }))
        .unwrap();
    select(&mut s, &[], &[b]);
    s.dispatch(Action::Paste).unwrap();
    assert!(starts(&s, b).is_empty());
    assert!(!s.project().sources.contains_key(&gone));
    assert!(
        s.latest_notice()
            .is_some_and(|n| n.text.contains("audio file is gone"))
    );
}

#[test]
fn a_copied_range_brings_its_automation_and_keeps_empty_tracks_in_place() {
    let Song {
        mut s,
        a,
        b,
        c,
        d,
        source,
        ..
    } = song();
    add(&mut s, audio_clip(a, source, 0.0, 4.0, 100));
    add(&mut s, audio_clip(c, source, 0.0, 4.0, 101));
    // A volume ride on A over the first bar.
    let lane = AutomationLane {
        id: faderframe_core::AutomationLaneId(7_000),
        target: AutomationTarget::TrackVolume,
        curve: AutomationCurve::from_points(vec![
            AutomationPoint {
                time: bar(0.0),
                value: -20.0,
                shape: Default::default(),
            },
            AutomationPoint {
                time: bar(1.0),
                value: 0.0,
                shape: Default::default(),
            },
        ]),
        mode: Default::default(),
        visible: true,
    };
    s.dispatch(Action::Edit(Command::AddAutomationLane {
        track: a,
        lane: Box::new(lane),
    }))
    .unwrap();
    // The range: the first bar of A, B and C.
    select(&mut s, &[], &[a, b, c]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(0.0),
        bar(1.0),
    ))))
    .unwrap();
    s.dispatch(Action::Copy).unwrap();
    assert_eq!(s.clipboard().kind(), ClipboardKind::Range);
    assert_eq!(s.clipboard().lanes().len(), 3);
    // Pasted with B selected: A's lane goes to B, B's (empty) lane keeps
    // its place on C (nothing changes there), and C's would land on M,
    // which takes no audio, so it goes on down to D.
    select(&mut s, &[], &[b]);
    s.dispatch(Action::SetEditRange(None)).unwrap();
    locate(&mut s, bar(8.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(layout(&s, b), vec![(8.0, 9.0)]);
    assert_eq!(layout(&s, c), vec![(0.0, 4.0)]);
    assert_eq!(layout(&s, d), vec![(8.0, 9.0)]);
    let vol = |s: &Session, t: TrackId, at: MusicalTime| {
        s.project()
            .track(t)
            .unwrap()
            .automation
            .lane(AutomationTarget::TrackVolume)
            .and_then(|l| l.curve.value_at(at))
    };
    // B got the volume ride, at its new place.
    assert_eq!(vol(&s, b, bar(8.0)), Some(-20.0));
    assert!((vol(&s, b, bar(8.5)).unwrap() + 10.0).abs() < 0.1);
    // A's own ride is unchanged.
    assert_eq!(vol(&s, a, bar(0.0)), Some(-20.0));
    // Undo takes the lane away again (one step).
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(vol(&s, b, bar(8.0)), None);
}

#[test]
fn duplicate_places_copies_end_to_end_and_continues_from_the_last() {
    let Song {
        mut s,
        a,
        c,
        source,
        ..
    } = song();
    let x = add(&mut s, audio_clip(a, source, 0.0, 1.0, 100));
    let y = add(&mut s, audio_clip(c, source, 0.5, 1.0, 101));
    select(&mut s, &[x, y], &[a, c]);
    s.dispatch(Action::Repeat(1)).unwrap();
    assert_eq!(s.history().undo_label(), Some("Duplicate"));
    // The selection spans 0 – 1.5 bars: the copies start 1.5 bars later.
    assert_eq!(starts(&s, a), vec![0.0, 1.5]);
    assert_eq!(starts(&s, c), vec![0.5, 2.0]);
    let copies: Vec<ClipId> = s.selection.clips.iter().copied().collect();
    assert_eq!(copies.len(), 2);
    assert!(!copies.contains(&x) && !copies.contains(&y));
    // Again: after the copies.
    s.dispatch(Action::Repeat(1)).unwrap();
    assert_eq!(starts(&s, a), vec![0.0, 1.5, 3.0]);
    // Three at once, one step.
    s.dispatch(Action::Repeat(3)).unwrap();
    assert_eq!(starts(&s, a), vec![0.0, 1.5, 3.0, 4.5, 6.0, 7.5]);
    assert_eq!(s.history().undo_label(), Some("Repeat"));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(starts(&s, a), vec![0.0, 1.5, 3.0]);
    // A repeated range over a long clip (it was cut once per copy).
    add(&mut s, audio_clip(c, source, 10.0, 8.0, 102));
    select(&mut s, &[], &[c]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(10.0),
        bar(11.0),
    ))))
    .unwrap();
    s.dispatch(Action::Repeat(3)).unwrap();
    assert_eq!(
        layout(&s, c)
            .into_iter()
            .filter(|(a, _)| *a >= 10.0)
            .collect::<Vec<_>>(),
        vec![
            (10.0, 11.0),
            (11.0, 12.0),
            (12.0, 13.0),
            (13.0, 14.0),
            (14.0, 18.0)
        ]
    );
    let p = s.project();
    for (i, cl) in p
        .clips_of(c)
        .iter()
        .filter(|cl| cl.start >= bar(11.0))
        .take(3)
        .enumerate()
    {
        assert_eq!(
            cl.as_audio().unwrap().source_offset,
            10 * BAR,
            "copy {i} plays bar 10"
        );
    }
}

#[test]
fn pasted_clips_are_saved_and_opened_again() {
    let Song {
        mut s,
        a,
        m,
        source,
        ..
    } = song();
    let x = add(&mut s, audio_clip(a, source, 0.0, 1.0, 100));
    let z = add(&mut s, midi_clip(m, 0.0, 101));
    select(&mut s, &[x, z], &[a, m]);
    s.dispatch(Action::Copy).unwrap();
    locate(&mut s, bar(4.0));
    s.dispatch(Action::Paste).unwrap();
    let pasted: Vec<Clip> = s
        .selection
        .clips
        .iter()
        .map(|c| s.project().clip(*c).unwrap().clone())
        .collect();
    let dir = std::env::temp_dir().join(format!("ff-clipboard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pasted.ffproj");
    s.save_as(&path).unwrap();
    let mut t = Session::new(Project::new("Other", SR), None, EngineConfig::default()).unwrap();
    t.open(&path).unwrap();
    for c in &pasted {
        assert_eq!(t.project().clip(c.id), Some(c));
        assert!(t.project().track(c.track).unwrap().clips.contains(&c.id));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn launcher_clips_are_never_copied() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let track = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let first = s.project().track(track).unwrap().clips[0];
    s.dispatch(Action::Launcher(
        faderframe_session::launcher::LauncherOp::SendClips(vec![first]),
    ))
    .unwrap();
    assert!(!s.project().launcher_clips().is_empty());
    let launcher = s.project().launcher_clips();
    let arranged = s.project().track(track).unwrap().clips.clone();
    let mut all: Vec<ClipId> = launcher.iter().copied().collect();
    all.extend(arranged.iter().copied());
    select(&mut s, &all, &[track]);
    s.dispatch(Action::Copy).unwrap();
    assert_eq!(s.clipboard().clip_count(), arranged.len());
    assert!(
        s.clipboard()
            .lanes()
            .iter()
            .flat_map(|l| &l.clips)
            .all(|c| !launcher.contains(&c.id))
    );
}

/// Time added or taken away on tracks takes their automation along: what
/// played after the edit point still plays with the audio it rode.
#[test]
fn insert_silence_and_shuffle_clears_move_the_automation_too() {
    let Song {
        mut s, a, source, ..
    } = song();
    add(&mut s, audio_clip(a, source, 0.0, 8.0, 100));
    let point = |at: f64, value: f64| AutomationPoint {
        time: bar(at),
        value,
        shape: Default::default(),
    };
    s.dispatch(Action::Edit(Command::AddAutomationLane {
        track: a,
        lane: Box::new(AutomationLane {
            id: faderframe_core::AutomationLaneId(7_001),
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::from_points(vec![point(0.0, -10.0), point(4.0, 0.0)]),
            mode: Default::default(),
            visible: true,
        }),
    }))
    .unwrap();
    let vol = |s: &Session, at: f64| {
        s.project()
            .track(a)
            .unwrap()
            .automation
            .lane(AutomationTarget::TrackVolume)
            .and_then(|l| l.curve.value_at(bar(at)))
            .unwrap()
    };
    select(&mut s, &[], &[a]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(2.0),
        bar(3.0),
    ))))
    .unwrap();
    s.dispatch(Action::InsertSilence).unwrap();
    assert_eq!(layout(&s, a), vec![(0.0, 2.0), (3.0, 9.0)]);
    // The value at bar 2 holds over the silence; the ride goes on later.
    assert!((vol(&s, 2.5) + 5.0).abs() < 1e-6);
    assert!((vol(&s, 4.0) + 2.5).abs() < 1e-6, "what was at 3 is at 4");
    assert!(vol(&s, 5.0).abs() < 1e-6, "the end of the ride a bar later");
    s.dispatch(Action::Undo).unwrap();
    assert!(vol(&s, 4.0).abs() < 1e-6);
    // Shuffle: clearing a bar closes the gap in the automation too.
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    select(&mut s, &[], &[a]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(2.0),
    ))))
    .unwrap();
    s.dispatch(Action::ClearRange).unwrap();
    assert_eq!(layout(&s, a), vec![(0.0, 1.0), (1.0, 7.0)]);
    assert!(vol(&s, 3.0).abs() < 1e-6, "the ride ends a bar earlier");
    assert!(
        (vol(&s, 0.5) + 8.75).abs() < 1e-6,
        "before the cut as it was"
    );
}

#[test]
fn a_cut_or_cleared_range_takes_its_automation_as_copy_does() {
    let Song {
        mut s,
        a,
        b,
        source,
        ..
    } = song();
    add(&mut s, audio_clip(a, source, 0.0, 8.0, 100));
    let point = |at: f64, value: f64| AutomationPoint {
        time: bar(at),
        value,
        shape: Default::default(),
    };
    // A ride on A: -10 dB at 0, up to 0 dB at 2, down to -20 dB at 4, back
    // to -6 dB at 6.
    s.dispatch(Action::Edit(Command::AddAutomationLane {
        track: a,
        lane: Box::new(AutomationLane {
            id: faderframe_core::AutomationLaneId(7_002),
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::from_points(vec![
                point(0.0, -10.0),
                point(2.0, 0.0),
                point(4.0, -20.0),
                point(6.0, -6.0),
            ]),
            mode: Default::default(),
            visible: true,
        }),
    }))
    .unwrap();
    let vol = |s: &Session, at: f64| {
        s.project()
            .track(a)
            .unwrap()
            .automation
            .lane(AutomationTarget::TrackVolume)
            .and_then(|l| l.curve.value_at(bar(at)))
            .unwrap()
    };
    let points = |s: &Session| {
        s.project()
            .track(a)
            .unwrap()
            .automation
            .lane(AutomationTarget::TrackVolume)
            .unwrap()
            .curve
            .points()
            .to_vec()
    };
    let last_step = |s: &Session| s.history_steps().0.last().cloned();
    let original = points(&s);
    let before: Vec<f64> = (0..=16).map(|i| vol(&s, f64::from(i) * 0.5)).collect();
    // Slip: Cut bars 1–3 (the peak at 2 is in it).
    select(&mut s, &[], &[a]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(3.0),
    ))))
    .unwrap();
    s.dispatch(Action::Cut).unwrap();
    assert_eq!(layout(&s, a), vec![(0.0, 1.0), (3.0, 8.0)]);
    assert_eq!(s.clipboard().kind(), ClipboardKind::Range);
    assert_eq!(last_step(&s).as_deref(), Some("Cut"));
    // The track no longer has the ride of the range (Copy took it): the
    // peak is gone, the values outside stay, the line runs straight
    // between the range's edges.
    assert!((vol(&s, 2.0) + 7.5).abs() < 1e-6, "{}", vol(&s, 2.0));
    for (i, want) in before.iter().enumerate() {
        let at = i as f64 * 0.5;
        if !(1.0..=3.0).contains(&at) {
            assert!((vol(&s, at) - want).abs() < 1e-6, "at bar {at}");
        }
    }
    // Pasting it back where it was restores the ride.
    s.dispatch(Action::SetEditRange(None)).unwrap();
    select(&mut s, &[], &[a]);
    locate(&mut s, bar(1.0));
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(layout(&s, a), vec![(0.0, 1.0), (1.0, 3.0), (3.0, 8.0)]);
    for (i, want) in before.iter().enumerate() {
        let at = i as f64 * 0.5;
        assert!((vol(&s, at) - want).abs() < 1e-4, "pasted back: bar {at}");
    }
    // Undo both: as it was, point for point.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(points(&s), original);
    assert_eq!(layout(&s, a), vec![(0.0, 8.0)]);
    // Delete with a range clears it the same way (Grid mode); a range
    // holding no point of a lane leaves that lane as it is.
    s.dispatch(Action::SetEditMode(EditMode::Grid)).unwrap();
    select(&mut s, &[], &[a, b]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(4.5),
        bar(5.5),
    ))))
    .unwrap();
    s.dispatch(Action::DeleteSelection).unwrap();
    assert_eq!(
        points(&s),
        original,
        "no point inside: the lane is as it was"
    );
    assert_eq!(layout(&s, a), vec![(0.0, 4.5), (5.5, 8.0)]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(3.0),
        bar(6.0),
    ))))
    .unwrap();
    s.dispatch(Action::DeleteSelection).unwrap();
    assert_eq!(last_step(&s).as_deref(), Some("Clear"));
    assert!((vol(&s, 3.0) - before[6]).abs() < 1e-6);
    assert!((vol(&s, 6.0) - before[12]).abs() < 1e-6);
    assert!((vol(&s, 7.0) + 6.0).abs() < 1e-6, "the last value holds on");
    assert!((vol(&s, 4.5) - (before[6] + before[12]) / 2.0).abs() < 1e-6);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(points(&s), original);
}
