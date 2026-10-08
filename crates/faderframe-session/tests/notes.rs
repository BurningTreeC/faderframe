#![allow(clippy::unwrap_used)]
//! Piano-roll note actions: one undo step each, ids from the session.

use faderframe_core::{ClipId, NoteId};
use faderframe_engine::EngineConfig;
use faderframe_project::midi_ops::{ChordKind, QuantizeSettings, Scale, ScaleKind};
use faderframe_project::{ControllerPoint, MidiClip, MidiController, MidiNote, Project, TrackKind};
use faderframe_session::{Action, NoteOp, Session};
use faderframe_timeline::{GridDivision, MusicalTime};

fn q(x: f64) -> MusicalTime {
    MusicalTime::from_quarters(x)
}

fn setup() -> (Session, ClipId) {
    let mut s = Session::new(Project::new("N", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters_i(16),
    })
    .unwrap();
    let clip = s.project().clips_of(t)[0].id;
    let notes: Vec<MidiNote> = [(0.1, 60), (1.05, 64), (2.2, 67)]
        .iter()
        .map(|&(start, key)| MidiNote {
            id: NoteId(0),
            start: q(start),
            length: q(0.4),
            key,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        })
        .collect();
    s.dispatch(Action::AddNotes { clip, notes }).unwrap();
    (s, clip)
}

fn clip(s: &Session, c: ClipId) -> MidiClip {
    s.project().clip(c).unwrap().as_midi().unwrap().clone()
}

#[test]
fn operations_apply_to_the_selection_and_undo_in_one_step() {
    let (mut s, c) = setup();
    let ids: Vec<NoteId> = clip(&s, c).notes.iter().map(|n| n.id).collect();
    assert_eq!(ids.len(), 3);
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![],
        op: NoteOp::Quantize(QuantizeSettings {
            grid: GridDivision::Note(4),
            ..Default::default()
        }),
    })
    .unwrap();
    let starts: Vec<MusicalTime> = clip(&s, c).notes.iter().map(|n| n.start).collect();
    assert_eq!(starts, vec![q(0.0), q(1.0), q(2.0)]);
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![ids[0]],
        op: NoteOp::Transpose(12),
    })
    .unwrap();
    assert_eq!(clip(&s, c).notes[0].key, 72);
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![],
        op: NoteOp::ToggleMuted,
    })
    .unwrap();
    assert!(clip(&s, c).notes.iter().all(|n| n.muted));
    for _ in 0..3 {
        s.dispatch(Action::Undo).unwrap();
    }
    let starts: Vec<MusicalTime> = clip(&s, c).notes.iter().map(|n| n.start).collect();
    assert_eq!(starts, vec![q(0.1), q(1.05), q(2.2)]);
}

#[test]
fn duplicate_split_copy_paste_and_chords() {
    let (mut s, c) = setup();
    let ids: Vec<NoteId> = clip(&s, c).notes.iter().map(|n| n.id).collect();
    // Duplicate after the span (whole beats: span 0.1..2.6 → 3 beats).
    s.dispatch(Action::DuplicateNotes {
        clip: c,
        notes: ids.clone(),
        offset: None,
        keys: 0,
    })
    .unwrap();
    let m = clip(&s, c);
    assert_eq!(m.notes.len(), 6);
    assert!(m.notes.iter().any(|n| n.start == q(3.1) && n.key == 60));
    assert_eq!(s.selection.notes.len(), 3, "copies selected");
    // Split the first note in the middle.
    s.dispatch(Action::SplitNotes {
        clip: c,
        notes: vec![ids[0]],
        at: q(0.3),
    })
    .unwrap();
    let m = clip(&s, c);
    assert_eq!(m.notes.len(), 7);
    assert_eq!(m.note(ids[0]).unwrap().end(), q(0.3));
    // Copy and paste at beat 8.
    s.dispatch(Action::CopyNotes {
        clip: c,
        notes: vec![ids[1], ids[2]],
    })
    .unwrap();
    s.dispatch(Action::PasteNotes {
        clip: c,
        at: q(8.0),
    })
    .unwrap();
    let m = clip(&s, c);
    assert!(m.notes.iter().any(|n| n.start == q(8.0) && n.key == 64));
    assert!(
        m.notes
            .iter()
            .any(|n| (n.start - q(9.15)).ticks().abs() < 2 && n.key == 67)
    );
    // A scale chord.
    let mut pr = s.editor.piano;
    pr.scale = Scale::new(0, ScaleKind::Major);
    pr.chord = ChordKind::ScaleTriad;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    s.dispatch(Action::AddChord {
        clip: c,
        start: q(12.0),
        length: q(1.0),
        key: 62,
        velocity: 90,
    })
    .unwrap();
    let mut keys: Vec<u8> = clip(&s, c)
        .notes
        .iter()
        .filter(|n| n.start == q(12.0))
        .map(|n| n.key)
        .collect();
    keys.sort();
    assert_eq!(keys, vec![62, 65, 69], "D minor in C major");
}

#[test]
fn controller_lanes_and_clip_length() {
    let (mut s, c) = setup();
    let pts: Vec<ControllerPoint> = (0..4)
        .map(|i| ControllerPoint {
            time: q(i as f64),
            value: 30 * i,
        })
        .collect();
    s.dispatch(Action::SetControllerPoints {
        clip: c,
        controller: MidiController::MOD_WHEEL,
        channel: 0,
        from: q(0.0),
        to: q(4.0),
        points: pts,
    })
    .unwrap();
    let lane = clip(&s, c)
        .lane(MidiController::MOD_WHEEL, 0)
        .unwrap()
        .clone();
    assert_eq!(lane.points.len(), 4);
    assert_eq!(lane.value_at(q(2.5)), Some(60));
    // Erase a range: points inside go.
    s.dispatch(Action::SetControllerPoints {
        clip: c,
        controller: MidiController::MOD_WHEEL,
        channel: 0,
        from: q(1.0),
        to: q(3.0),
        points: vec![],
    })
    .unwrap();
    let lane = clip(&s, c)
        .lane(MidiController::MOD_WHEEL, 0)
        .unwrap()
        .clone();
    assert_eq!(
        lane.points.iter().map(|p| p.value).collect::<Vec<_>>(),
        vec![0, 90]
    );
    s.dispatch(Action::SetMidiClipLength {
        clip: c,
        length: q(32.0),
    })
    .unwrap();
    assert_eq!(clip(&s, c).length, q(32.0));
}

#[test]
fn expression_travels_with_notes() {
    use faderframe_project::{ExpressionKind, ExpressionPoint};
    let (mut s, c) = setup();
    let first = clip(&s, c).notes[0].id;
    // A pitch glide over the first note (0.4 quarters long).
    s.dispatch(Action::SetNoteExpression {
        clip: c,
        note: first,
        kind: ExpressionKind::Pitch,
        from: MusicalTime::ZERO,
        to: q(1.0),
        points: vec![
            ExpressionPoint {
                time: MusicalTime::ZERO,
                value: 0.0,
            },
            ExpressionPoint {
                time: q(0.4),
                value: 4.0,
            },
        ],
    })
    .unwrap();
    let glide = |s: &Session, id: NoteId, t: f64| {
        clip(s, c)
            .expression(id)
            .map(|e| e.value_at(ExpressionKind::Pitch, q(t)))
    };
    assert_eq!(glide(&s, first, 0.2), Some(2.0));
    assert_eq!(s.history().undo_label(), Some("Edit Expression"));
    // Moving and transposing keep it (same note).
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![first],
        op: NoteOp::Move {
            by: q(4.0),
            keys: 12,
        },
    })
    .unwrap();
    assert_eq!(glide(&s, first, 0.2), Some(2.0));
    // A duplicate gets a copy.
    s.dispatch(Action::DuplicateNotes {
        clip: c,
        notes: vec![first],
        offset: Some(q(1.0)),
        keys: 0,
    })
    .unwrap();
    let copy = *s.selection.notes.first().unwrap();
    assert_ne!(copy, first);
    assert_eq!(glide(&s, copy, 0.4), Some(4.0));
    // Split: the right part continues from the cut.
    let start = clip(&s, c).note(copy).unwrap().start;
    s.dispatch(Action::SplitNotes {
        clip: c,
        notes: vec![copy],
        at: start + q(0.2),
    })
    .unwrap();
    let right = clip(&s, c)
        .notes
        .iter()
        .find(|n| n.start == start + q(0.2))
        .unwrap()
        .id;
    assert_eq!(glide(&s, right, 0.0), Some(2.0));
    assert_eq!(glide(&s, right, 0.2), Some(4.0));
    // Copy and paste take it along; deleting drops it.
    s.dispatch(Action::CopyNotes {
        clip: c,
        notes: vec![first],
    })
    .unwrap();
    let pasted = {
        s.dispatch(Action::PasteNotes {
            clip: c,
            at: q(12.0),
        })
        .unwrap();
        *s.selection.notes.first().unwrap()
    };
    assert_eq!(glide(&s, pasted, 0.2), Some(2.0));
    s.dispatch(Action::RemoveNotes {
        clip: c,
        notes: vec![pasted],
    })
    .unwrap();
    assert!(clip(&s, c).expression(pasted).is_none());
}

fn notes_of(s: &Session, clip: ClipId) -> Vec<MidiNote> {
    let mut n = s
        .project()
        .clip(clip)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone();
    n.sort_by_key(|n| (n.start, n.key));
    n
}

/// MIDI Tools: what the panel previews is what applying does, in one
/// undo step; generators add to the clip or replace what is there.
#[test]
fn midi_tools_preview_and_apply_in_one_step() {
    use faderframe_project::midi_tools::Tool;
    let (mut s, clip) = setup();
    assert!(s.preview_midi_tool(clip, &[]).is_none(), "no tool chosen");
    // Chop every note in four.
    let mut pr = s.editor.piano;
    pr.tool = Some(Tool::Chop);
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    let preview = s.preview_midi_tool(clip, &[]).unwrap();
    assert_eq!(preview.notes.len(), 12);
    assert_eq!(preview.removed.len(), 3);
    s.dispatch(Action::ApplyMidiTool {
        clip,
        notes: Vec::new(),
    })
    .unwrap();
    let after = notes_of(&s, clip);
    assert_eq!(after.len(), 12);
    let mut expected = preview.notes.clone();
    expected.sort_by_key(|n| (n.start, n.key));
    for (a, b) in after.iter().zip(&expected) {
        assert_eq!(
            (a.start, a.length, a.key, a.velocity),
            (b.start, b.length, b.key, b.velocity)
        );
    }
    assert!(after.iter().all(|n| n.id.0 != 0), "ids from the session");
    assert_eq!(s.selection.notes.len(), 12, "what it made is selected");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(notes_of(&s, clip).len(), 3);
    // A drum pattern over the whole clip, replacing what is there.
    pr.tool = Some(Tool::Drums);
    pr.tools.replace = true;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    let p = s.preview_midi_tool(clip, &[]).unwrap();
    assert_eq!(p.range, Some((MusicalTime::ZERO, q(16.0))));
    assert_eq!(p.removed.len(), 3);
    s.dispatch(Action::ApplyMidiTool {
        clip,
        notes: Vec::new(),
    })
    .unwrap();
    let after = notes_of(&s, clip);
    assert_eq!(
        after.iter().filter(|n| n.key == 36).count(),
        16,
        "four bars of four kicks"
    );
    assert!(after.iter().all(|n| [36, 39, 42, 46].contains(&n.key)));
    // Generating from a selection: the bars it spans (bar 2 here).
    s.dispatch(Action::Undo).unwrap();
    pr.tools.replace = false;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    let second = notes_of(&s, clip)
        .into_iter()
        .find(|n| n.key == 67)
        .unwrap();
    s.dispatch(Action::NoteOperation {
        clip,
        notes: vec![second.id],
        op: NoteOp::Move {
            by: q(3.0),
            keys: 0,
        },
    })
    .unwrap();
    let moved = notes_of(&s, clip)
        .into_iter()
        .find(|n| n.key == 67)
        .unwrap();
    assert_eq!(moved.start, q(5.2));
    let p = s.preview_midi_tool(clip, &[moved.id]).unwrap();
    let bar = (q(4.0), q(8.0));
    assert_eq!(p.range, Some(bar));
    assert!(p.notes.iter().all(|n| n.start >= bar.0 && n.start < bar.1));
    assert!(p.removed.is_empty(), "added, not replacing");
}
