//! The arranger's editing audit (§2.9): ordinary edits that used to go
//! wrong — Delete reaching the piano roll's notes, vertical moves counting
//! hidden tracks, launcher clips edited as arrangement clips, range edits
//! refused near clip edges, Shuffle trims of several clips overlapping,
//! nudge-trims refused for a take folder, start trims reaching into a
//! clip-effects render's lead-in.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::launcher::SlotKey;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MidiClip, MidiNote, Project,
    SourceSpec, StretchSettings, TrackKind,
};
use faderframe_session::editing::{ClipEdge, EditMode, NudgeTarget};
use faderframe_session::launcher::LauncherOp;
use faderframe_session::{Action, EditRange, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// Frames per bar at 120 BPM in 4/4.
const BAR: i64 = 2 * SR as i64;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

fn session(tracks: &[TrackKind]) -> (Session, Vec<TrackId>, AudioSourceId) {
    let mut s = Session::new(Project::new("Audit", SR), None, EngineConfig::default()).unwrap();
    let ids = tracks.iter().map(|k| s.add_track(*k).unwrap()).collect();
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
    (s, ids, source)
}

fn audio(t: TrackId, source: AudioSourceId, start: MusicalTime, bars: f64, id: u64) -> Clip {
    Clip {
        id: ClipId(id),
        track: t,
        name: format!("clip {id}"),
        color: None,
        start,
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source,
            source_offset: BAR,
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

fn midi(t: TrackId, start: MusicalTime, id: u64) -> Clip {
    let note = |n: u64, at: f64| MidiNote {
        id: NoteId(n),
        start: MusicalTime::from_quarters(at),
        length: MusicalTime::from_quarters(0.5),
        key: 60,
        velocity: 90,
        channel: 0,
        muted: false,
        release: None,
    };
    Clip {
        id: ClipId(id),
        track: t,
        name: "keys".into(),
        color: None,
        start,
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: bar(1.0),
            notes: vec![note(id * 10 + 1, 0.0), note(id * 10 + 2, 1.0)],
            ..Default::default()
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

fn notes(s: &Session, clip: ClipId) -> usize {
    s.project()
        .clip(clip)
        .and_then(|c| c.as_midi())
        .map_or(0, |m| m.notes.len())
}

/// Notes selected in the piano roll and a clip selected in the arranger:
/// the arranger's Delete deletes the clip, never the notes; with a range
/// it clears the range. (Control surfaces' Delete keeps its order: the
/// range, the notes, the clips.)
#[test]
fn the_arrangers_delete_deletes_its_clips_not_the_piano_rolls_notes() {
    let (mut s, t, source) = session(&[TrackKind::Instrument, TrackKind::Audio]);
    let keys = add(&mut s, midi(t[0], bar(0.0), 100));
    let drums = add(&mut s, audio(t[1], source, bar(0.0), 2.0, 101));
    s.dispatch(Action::OpenClipEditor(keys)).unwrap();
    s.dispatch(Action::SelectNotes {
        notes: vec![NoteId(1001)],
        mode: SelectMode::Replace,
    })
    .unwrap();
    // The user clicks the drum clip in the arranger and presses Delete.
    select(&mut s, &[drums], &[t[1]]);
    assert!(!s.selection.notes.is_empty(), "the notes stay selected");
    s.dispatch(Action::DeleteInArranger).unwrap();
    assert_eq!(layout(&s, t[1]), vec![], "the drum clip is gone");
    assert_eq!(notes(&s, keys), 2, "the notes are untouched");
    s.dispatch(Action::Undo).unwrap();
    // A surface's Delete: the notes first, as ever.
    select(&mut s, &[drums], &[t[1]]);
    s.dispatch(Action::SelectNotes {
        notes: vec![NoteId(1001)],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::DeleteSelection).unwrap();
    assert_eq!(notes(&s, keys), 1);
    assert_eq!(layout(&s, t[1]), vec![(0.0, 2.0)]);
    // With a range the arranger's Delete clears it.
    select(&mut s, &[], &[t[1]]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(0.5),
        bar(1.0),
    ))))
    .unwrap();
    s.dispatch(Action::DeleteInArranger).unwrap();
    assert_eq!(layout(&s, t[1]), vec![(0.0, 0.5), (1.0, 2.0)]);
}

/// A clip dragged rows down lands on the row the arranger shows there:
/// rows are in folder order and a closed folder's tracks are not rows.
#[test]
fn a_vertical_move_counts_the_rows_the_arranger_shows() {
    let kinds = [TrackKind::Audio; 4];
    let (mut s, t, source) = session(&kinds);
    let (a, b, c, d) = (t[0], t[1], t[2], t[3]);
    let clip = add(&mut s, audio(a, source, bar(0.0), 1.0, 100));
    s.dispatch(Action::NewFolder { tracks: vec![b, c] })
        .unwrap();
    let folder = s
        .project()
        .tracks
        .iter()
        .find(|x| x.kind == TrackKind::Folder)
        .unwrap()
        .id;
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    assert_eq!(s.arranger_rows(), vec![a, folder, d], "B and C are hidden");
    // Two rows down: D (not B, the second track in the project's list).
    let move_by = |s: &mut Session, rows: i32| {
        s.dispatch(Action::BeginGesture("Move Clip".into()))
            .unwrap();
        s.dispatch(Action::MoveClips {
            clips: vec![clip],
            by: 0,
            tracks: rows,
        })
        .unwrap();
        s.dispatch(Action::EndGesture).unwrap();
    };
    move_by(&mut s, 2);
    assert_eq!(s.project().clip(clip).unwrap().track, d);
    assert_eq!(layout(&s, d), vec![(0.0, 1.0)]);
    assert!(s.project().track(b).unwrap().clips.is_empty());
    // One row up from D is the folder: no clips there, it stays.
    move_by(&mut s, -1);
    assert_eq!(s.project().clip(clip).unwrap().track, d);
    // Open: rows A, F, B, C, D — two up from D is B.
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    move_by(&mut s, -2);
    assert_eq!(s.project().clip(clip).unwrap().track, b);
}

/// A launcher clip in the selection (selected in the launcher, Ctrl-clicked
/// next to arrangement clips) is not split, nudged or deleted as an
/// arrangement clip: it stays in its slot, out of every track's list.
#[test]
fn launcher_clips_in_the_selection_stay_out_of_arrangement_edits() {
    let (mut s, t, source) = session(&[TrackKind::Audio]);
    let arranged = add(&mut s, audio(t[0], source, bar(0.0), 4.0, 100));
    s.dispatch(Action::Launcher(LauncherOp::SendClips(vec![arranged])))
        .unwrap();
    let slot = *s.project().launcher.slots.values().next().unwrap();
    let key: SlotKey = *s.project().launcher.slots.keys().next().unwrap();
    let slot_len = |s: &Session| {
        s.project()
            .clip(slot)
            .and_then(|c| c.as_audio())
            .map(|a| a.length)
    };
    let len = slot_len(&s);
    select(&mut s, &[arranged, slot], &[t[0]]);
    s.dispatch(Action::Transport(TransportAction::Locate(bar(1.0))))
        .unwrap();
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 1.0), (1.0, 4.0)]);
    assert_eq!(slot_len(&s), len, "the slot's clip is not split");
    assert!(!s.project().track(t[0]).unwrap().clips.contains(&slot));
    // Nudged: the arrangement's clips move, the slot's stays at zero.
    s.dispatch(Action::Nudge {
        forward: true,
        target: NudgeTarget::Move,
    })
    .unwrap();
    assert_eq!(s.project().clip(slot).unwrap().start, MusicalTime::ZERO);
    // Deleted from the arranger: the slot keeps its clip.
    s.dispatch(Action::DeleteInArranger).unwrap();
    assert_eq!(layout(&s, t[0]), vec![]);
    assert_eq!(s.project().launcher.slots.get(&key), Some(&slot));
    assert!(s.project().clip(slot).is_some());
}

/// A range edge less than a sample inside a clip (grid positions are not
/// whole samples) neither refuses the edit nor leaves a sliver: Clear,
/// Cut and Trim to Selection treat the clip as starting there.
#[test]
fn range_edits_a_hair_inside_a_clip_edge_go_through() {
    let (mut s, t, source) = session(&[TrackKind::Audio]);
    add(&mut s, audio(t[0], source, bar(1.0), 3.0, 100));
    // A quarter of a sample after the clip's start (40 ticks a sample).
    let a = bar(1.0) + MusicalTime::from_ticks(10);
    select(&mut s, &[], &[t[0]]);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(a, bar(2.0)))))
        .unwrap();
    s.dispatch(Action::ClearRange).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(2.0, 4.0)]);
    s.dispatch(Action::Undo).unwrap();
    // Trim to Selection: the start stays, the end goes.
    s.dispatch(Action::SetEditRange(Some(EditRange::new(a, bar(3.0)))))
        .unwrap();
    s.dispatch(Action::TrimToSelection).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(1.0, 3.0)]);
    s.dispatch(Action::Undo).unwrap();
    // A range ending a hair before the clip's end, Shuffle: the clip's
    // whole tail goes and nothing is left to move.
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    let b = bar(4.0) - MusicalTime::from_ticks(10);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(bar(3.0), b))))
        .unwrap();
    s.dispatch(Action::ClearRange).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(1.0, 3.0)]);
}

/// Shuffle: trimming the ends of several clips on one track adds up —
/// each clip moves by what the trims before it added, nothing overlaps.
#[test]
fn shuffle_trims_of_several_clips_add_up() {
    let (mut s, t, source) = session(&[TrackKind::Audio]);
    let a = add(&mut s, audio(t[0], source, bar(0.0), 4.0, 100));
    let b = add(&mut s, audio(t[0], source, bar(4.0), 4.0, 101));
    add(&mut s, audio(t[0], source, bar(8.0), 4.0, 102));
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    select(&mut s, &[a, b], &[t[0]]);
    let trim = |s: &mut Session, edge, by: f64| {
        s.dispatch(Action::BeginGesture("Trim Clip".into()))
            .unwrap();
        s.dispatch(Action::TrimClips {
            clips: vec![a, b],
            edge,
            by: bar(by).ticks(),
            stretch: false,
        })
        .unwrap();
        s.dispatch(Action::EndGesture).unwrap();
    };
    trim(&mut s, ClipEdge::End, -1.0);
    assert_eq!(layout(&s, t[0]), vec![(0.0, 3.0), (3.0, 6.0), (6.0, 10.0)]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 4.0), (4.0, 8.0), (8.0, 12.0)]);
    // Start trims shorten the clips where they stand; what follows closes up.
    trim(&mut s, ClipEdge::Start, 1.0);
    assert_eq!(layout(&s, t[0]), vec![(0.0, 3.0), (3.0, 6.0), (6.0, 10.0)]);
    s.dispatch(Action::Undo).unwrap();
    // Nudge-trims of both ends add up the same way.
    s.dispatch(Action::Nudge {
        forward: true,
        target: NudgeTarget::TrimEnd,
    })
    .unwrap();
    let l = layout(&s, t[0]);
    assert_eq!(l.len(), 3);
    assert!(l.windows(2).all(|w| w[0].1 <= w[1].0 + 1e-9), "{l:?}");
    assert!(
        (l[1].0 - l[0].1).abs() < 1e-9 && (l[2].0 - l[1].1).abs() < 1e-9,
        "{l:?}"
    );
    assert!(l[2].0 > 8.0, "C moved by both nudges: {l:?}");
}

/// A take folder among the selected clips: a nudge-trim trims the others
/// (folders are not trimmed, as by a drag) instead of refusing them all.
#[test]
fn a_nudge_trim_passes_over_take_folders() {
    let (mut s, t, source) = session(&[TrackKind::Audio, TrackKind::Audio]);
    let plain = add(&mut s, audio(t[0], source, bar(0.0), 2.0, 100));
    let mut folder = audio(t[1], source, bar(0.0), 2.0, 101);
    let mut takes = faderframe_project::TakeFolder::new(2 * BAR);
    let first = takes.add_take(faderframe_project::Take {
        name: "Take 1".into(),
        source,
        source_offset: 0,
        start: 0,
        end: 2 * BAR,
        gain_db: 0.0,
        rating: 0,
    });
    takes.use_take(first);
    folder.content = ClipContent::Takes(takes);
    let folder = add(&mut s, folder);
    select(&mut s, &[plain, folder], &[t[0], t[1]]);
    s.dispatch(Action::Nudge {
        forward: false,
        target: NudgeTarget::TrimEnd,
    })
    .unwrap();
    assert!(layout(&s, t[0])[0].1 < 2.0, "the plain clip is trimmed");
    assert_eq!(layout(&s, t[1]), vec![(0.0, 2.0)], "the folder is not");
}

/// A clip with clip effects plays a render that starts with the devices'
/// latency: trimming its start out never reaches into that lead-in.
#[test]
fn a_start_trim_stops_at_a_clip_effects_renders_start() {
    let (mut s, t, source) = session(&[TrackKind::Audio]);
    let mut c = audio(t[0], source, bar(2.0), 2.0, 100);
    let lead = 512;
    if let ClipContent::Audio(a) = &mut c.content {
        a.source_offset = lead;
        a.effects = Some(Box::new(faderframe_project::clip_fx::ClipEffects {
            chain: Vec::new(),
            original: faderframe_project::clip_fx::OriginalAudio::of(a),
            rendered_offset: lead,
        }));
    }
    let clip = add(&mut s, c);
    s.dispatch(Action::BeginGesture("Trim Clip".into()))
        .unwrap();
    s.dispatch(Action::TrimClips {
        clips: vec![clip],
        edge: ClipEdge::Start,
        by: -bar(1.0).ticks(),
        stretch: false,
    })
    .unwrap();
    s.dispatch(Action::EndGesture).unwrap();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_eq!(a.source_offset, lead, "the render's start, not before it");
    assert_eq!(a.length, 2 * BAR);
}

/// A section move would rearrange a frozen track's clips while its frozen
/// audio stays put: refused, as other edits of a frozen track are. One
/// that leaves the frozen track as it is goes through.
#[test]
fn a_section_move_does_not_rearrange_a_frozen_track() {
    let (mut s, t, source) = session(&[TrackKind::Audio, TrackKind::Audio]);
    // The frozen track has a clip in bars 0–2; the other one in 4–8.
    add(&mut s, audio(t[0], source, bar(0.0), 2.0, 100));
    add(&mut s, audio(t[1], source, bar(4.0), 4.0, 101));
    s.dispatch(Action::FreezeTrack(t[0])).unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !s.bouncing().is_empty() {
        assert!(std::time::Instant::now() < end, "freeze timed out");
        s.tick(0.016);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.project().track(t[0]).unwrap().freeze.is_some());
    s.dispatch(Action::AddSection {
        start: bar(6.0),
        end: bar(8.0),
    })
    .unwrap();
    s.dispatch(Action::AddSection {
        start: bar(0.0),
        end: bar(2.0),
    })
    .unwrap();
    let section = |s: &Session, at: MusicalTime| {
        s.project()
            .sections
            .iter()
            .find(|x| x.start == at)
            .unwrap()
            .id
    };
    // Bars 6–8 to bar 4: only the unfrozen track's material moves.
    let late = section(&s, bar(6.0));
    s.dispatch(Action::MoveSection {
        section: late,
        to: bar(4.0),
        copy: false,
    })
    .unwrap();
    assert_eq!(layout(&s, t[0]), vec![(0.0, 2.0)]);
    assert_eq!(layout(&s, t[1]).len(), 2, "the other track rearranged");
    // Bars 0–2 to bar 6: the frozen clip would move — refused, nothing
    // changes.
    let before = s.project().clone();
    let early = section(&s, bar(0.0));
    let refused = s.dispatch(Action::MoveSection {
        section: early,
        to: bar(6.0),
        copy: false,
    });
    assert!(refused.is_err(), "refused: {refused:?}");
    assert_eq!(s.project().clips, before.clips);
    assert_eq!(s.project().sections, before.sections);
}
