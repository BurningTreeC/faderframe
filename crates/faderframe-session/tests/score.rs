//! The score editor: typed and pointed entry writes the clip's notes (the
//! piano roll's), each edit one undo step; the score reads them back as it
//! wrote them; durations, ties, rests, chords, voices, accidentals by key,
//! respelling, dynamics and articulations played as velocity and length,
//! syllables, chord symbols, display quantise and quantising to it, export.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_leadsheet::score::{Syllabic, Value};
use faderframe_midi::theory::{Key, Scale};
use faderframe_project::notation::{Dynamic, Mark, ScoreGrid};
use faderframe_project::{
    Clip, ClipContent, Command, KeyChange, MidiClip, MidiNote, Project, TrackKind,
};
use faderframe_session::score::{
    Interval, ScoreCursor, ScoreKey, ScoreLayout, ScoreOf, ScoreOp, ScoreSettings,
};
use faderframe_session::{Action, NoteOp, SelectMode, Session, StepInput};
use faderframe_timeline::MusicalTime;

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

fn note(id: u64, start: f64, length: f64, key: u8) -> MidiNote {
    MidiNote {
        id: NoteId(id),
        start: q(start),
        length: q(length),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
        articulation: None,
        fine: None,
    }
}

/// A session with an instrument track `name` holding a clip of `bars` 4/4
/// bars at the start with `notes`, shown in the score.
fn with_clip(name: &str, bars: f64, notes: Vec<MidiNote>) -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Score", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.edit(Command::RenameTrack {
        track: t,
        name: name.into(),
    })
    .unwrap();
    let id = ClipId(80_001);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "tune".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: q(4.0 * bars),
                notes,
                ..MidiClip::default()
            }),
        }),
    })
    .unwrap();
    s.dispatch(Action::Score(ScoreOp::Show(ScoreOf::Clip(id))))
        .unwrap();
    (s, t, id)
}

fn entering(s: &mut Session) {
    let settings = ScoreSettings {
        entering: true,
        ..s.score_settings()
    };
    s.dispatch(Action::Score(ScoreOp::Settings(settings)))
        .unwrap();
}

/// The clip's notes: (start, length, key) in quarters, by start then key.
fn notes(s: &Session, c: ClipId) -> Vec<(f64, f64, u8)> {
    let mut out: Vec<(f64, f64, u8)> = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start.quarters(), n.length.quarters(), n.key))
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
    out
}

fn id_at(s: &Session, c: ClipId, start: f64) -> NoteId {
    s.project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .find(|n| (n.start.quarters() - start).abs() < 1e-6)
        .unwrap()
        .id
}

#[test]
fn a_typed_line_is_the_clips_notes_and_reads_back_as_quarters() {
    let (mut s, _, c) = with_clip("Lead", 2.0, Vec::new());
    entering(&mut s);
    s.score_keys("4 c d e f g a b c").unwrap();
    // The first C near the staff's middle (C5), each letter the nearest
    // octave to the one before.
    let keys: Vec<u8> = notes(&s, c).iter().map(|n| n.2).collect();
    assert_eq!(keys, vec![72, 74, 76, 77, 79, 81, 83, 84]);
    assert!(
        notes(&s, c)
            .iter()
            .enumerate()
            .all(|(i, n)| (n.0 - i as f64).abs() < 1e-9 && (n.1 - 1.0).abs() < 1e-9)
    );
    let doc = s.score().unwrap();
    let bar = &doc.sheet.parts[0].measures[0];
    assert!(
        bar.events
            .iter()
            .all(|e| e.duration.value == Value::Quarter)
    );
    // The cursor waits after the last; each note was one step.
    assert_eq!(s.score_cursor().unwrap().at, q(8.0));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(notes(&s, c).len(), 7);
}

#[test]
fn values_dots_rests_and_ties() {
    let (mut s, _, c) = with_clip("Lead", 2.0, Vec::new());
    entering(&mut s);
    s.score_keys("8 c d . e 16 f").unwrap();
    let got: Vec<(f64, f64)> = notes(&s, c).iter().map(|n| (n.0, n.1)).collect();
    assert_eq!(got, vec![(0.0, 0.5), (0.5, 0.5), (1.0, 0.75), (1.75, 0.25)]);
    let (mut s, _, c) = with_clip("Lead", 2.0, Vec::new());
    entering(&mut s);
    s.score_keys("4 c r d ~").unwrap();
    let got: Vec<(f64, f64)> = notes(&s, c).iter().map(|n| (n.0, n.1)).collect();
    assert_eq!(got, vec![(0.0, 1.0), (2.0, 2.0)]);
    assert_eq!(s.score_cursor().unwrap().at, q(4.0));
    let doc = s.score().unwrap();
    let bar = &doc.sheet.parts[0].measures[0];
    assert!(bar.events[1].is_rest());
    assert_eq!(bar.events[2].duration.value, Value::Half);
}

#[test]
fn upper_case_letters_build_a_chord_above() {
    let (mut s, _, c) = with_clip("Lead", 1.0, Vec::new());
    entering(&mut s);
    s.score_keys("4 c E G d").unwrap();
    assert_eq!(
        notes(&s, c),
        vec![
            (0.0, 1.0, 72),
            (0.0, 1.0, 76),
            (0.0, 1.0, 79),
            (1.0, 1.0, 74)
        ]
    );
    let doc = s.score().unwrap();
    assert_eq!(doc.sheet.parts[0].measures[0].events[0].chord.len(), 2);
}

/// Letters take the key's accidentals; a semitone up is spelt sharp, the
/// key's own note as the key has it, a semitone down flat.
#[test]
fn letters_follow_the_key_and_arrows_spell_by_direction() {
    let (mut s, _, c) = with_clip("Lead", 1.0, Vec::new());
    s.edit(Command::SetKeys {
        keys: vec![KeyChange {
            at: MusicalTime::ZERO,
            key: Key::new(7, Scale::Major),
        }],
    })
    .unwrap();
    entering(&mut s);
    s.score_keys("4 f").unwrap();
    assert_eq!(notes(&s, c)[0].2, 66, "F♯4 in G (three steps under B4)");
    s.score_keys("v").unwrap();
    assert_eq!(notes(&s, c)[0].2, 65);
    let doc = s.score().unwrap();
    let id = id_at(&s, c, 0.0);
    assert_eq!(doc.spelt(id).map(|(st, al, _)| (st, al)), Some((3, 0)));
    s.score_keys("v").unwrap();
    let doc = s.score().unwrap();
    // E in G: the key's note, no override.
    assert_eq!(doc.spelt(id).map(|(st, al, _)| (st, al)), Some((2, 0)));
    s.score_keys("v").unwrap();
    let doc = s.score().unwrap();
    assert_eq!(
        doc.spelt(id).map(|(st, al, _)| (st, al)),
        Some((2, -1)),
        "E♭"
    );
    // Respelt: F𝄫, then D♯ (the key's own way, nothing kept).
    s.score_keys("j").unwrap();
    let doc = s.score().unwrap();
    assert_eq!(doc.spelt(id).map(|(st, al, _)| (st, al)), Some((3, -2)));
    s.score_keys("j").unwrap();
    let doc = s.score().unwrap();
    assert_eq!(doc.spelt(id).map(|(st, al, _)| (st, al)), Some((1, 1)));
    assert!(s.project().clip_notation(c).notes.is_empty());
}

/// Entering over notes replaces what the voice had there.
#[test]
fn entry_replaces_what_its_voice_had() {
    let (mut s, _, c) = with_clip("Lead", 1.0, Vec::new());
    entering(&mut s);
    s.score_keys("4 c d e f").unwrap();
    s.dispatch(Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
        part: 0,
        at: MusicalTime::ZERO,
    }))))
    .unwrap();
    s.score_keys("2 g").unwrap();
    let got: Vec<(f64, f64)> = notes(&s, c).iter().map(|n| (n.0, n.1)).collect();
    assert_eq!(got, vec![(0.0, 2.0), (2.0, 1.0), (3.0, 1.0)]);
}

/// A second voice entered under a line keeps the line: the score writes
/// them on one staff, the line's stems up.
#[test]
fn a_second_voice_is_entered_under_the_first() {
    let (mut s, _, c) = with_clip("Lead", 1.0, Vec::new());
    entering(&mut s);
    s.score_keys("4 e f g a").unwrap();
    s.dispatch(Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
        part: 0,
        at: MusicalTime::ZERO,
    }))))
    .unwrap();
    s.score_keys("v2 1 c").unwrap();
    assert_eq!(notes(&s, c).len(), 5);
    let doc = s.score().unwrap();
    let bar = &doc.sheet.parts[0].measures[0];
    assert_eq!(bar.events.len(), 4);
    assert_eq!(bar.under.len(), 1);
    assert_eq!(bar.under[0].duration.value, Value::Whole);
    let whole = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .find(|n| n.length == q(4.0))
        .unwrap()
        .id;
    assert_eq!(s.project().clip_notation(c).note(whole).voice, Some(2));
}

/// Outside note input a value key sets the selected notes' durations; the
/// notes their voice had inside the new length go.
#[test]
fn a_value_key_changes_the_selection() {
    let (mut s, _, c) = with_clip(
        "Lead",
        1.0,
        vec![
            note(1, 0.0, 1.0, 60),
            note(2, 1.0, 1.0, 62),
            note(3, 2.0, 1.0, 64),
        ],
    );
    s.dispatch(Action::SelectNotes {
        notes: vec![NoteId(1)],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.score_keys("2").unwrap();
    assert_eq!(notes(&s, c), vec![(0.0, 2.0, 60), (2.0, 1.0, 64)]);
    // One step.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(notes(&s, c).len(), 3);
}

#[test]
fn dynamics_set_velocities_until_the_next() {
    let (mut s, _, c) = with_clip(
        "Lead",
        2.0,
        (0..8).map(|i| note(10 + i, i as f64, 1.0, 60)).collect(),
    );
    s.dispatch(Action::Score(ScoreOp::Dynamic {
        notes: vec![NoteId(12)],
        dynamic: Some(Dynamic::Mf),
    }))
    .unwrap();
    s.dispatch(Action::Score(ScoreOp::Dynamic {
        notes: vec![NoteId(15)],
        dynamic: Some(Dynamic::F),
    }))
    .unwrap();
    let v: Vec<u8> = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .map(|n| n.velocity)
        .collect();
    assert_eq!(v, vec![100, 100, 80, 80, 80, 96, 96, 96]);
    // Marked again before the forte: up to it only.
    s.dispatch(Action::Score(ScoreOp::Dynamic {
        notes: vec![NoteId(10)],
        dynamic: Some(Dynamic::P),
    }))
    .unwrap();
    let v: Vec<u8> = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .map(|n| n.velocity)
        .collect();
    assert_eq!(v, vec![49, 49, 80, 80, 80, 96, 96, 96]);
    let doc = s.score().unwrap();
    let e = &doc.sheet.parts[0].measures[0].events[2];
    assert_eq!(e.dynamic.map(|d| d.name()), Some("mf"));
    // A note entered after the forte is played forte.
    entering(&mut s);
    s.dispatch(Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
        part: 0,
        at: q(7.0),
    }))))
    .unwrap();
    s.score_keys("c").unwrap();
    let last = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .max_by_key(|n| n.start)
        .unwrap()
        .velocity;
    assert_eq!(last, 96);
}

/// Staccato plays half the written value, the score still writes it whole
/// with its dot; accents play louder.
#[test]
fn articulations_are_played_and_written() {
    let (mut s, _, c) = with_clip(
        "Lead",
        1.0,
        vec![note(1, 0.0, 1.0, 67), note(2, 1.0, 1.0, 69)],
    );
    s.dispatch(Action::Score(ScoreOp::Mark {
        notes: vec![NoteId(1)],
        mark: Mark::Staccato,
        on: true,
    }))
    .unwrap();
    s.dispatch(Action::Score(ScoreOp::Mark {
        notes: vec![NoteId(2)],
        mark: Mark::Accent,
        on: true,
    }))
    .unwrap();
    assert_eq!(notes(&s, c), vec![(0.0, 0.5, 67), (1.0, 1.0, 69)]);
    let m = s.project().clip(c).unwrap().as_midi().unwrap();
    assert_eq!(m.note(NoteId(2)).unwrap().velocity, 120);
    let doc = s.score().unwrap();
    let bar = &doc.sheet.parts[0].measures[0];
    assert_eq!(bar.events[0].duration.value, Value::Quarter);
    assert!(bar.events[0].marks.staccato);
    assert!(bar.events[1].marks.accent);
    // Off again: the written value back.
    s.dispatch(Action::Score(ScoreOp::Mark {
        notes: vec![NoteId(1)],
        mark: Mark::Staccato,
        on: false,
    }))
    .unwrap();
    assert_eq!(notes(&s, c)[0], (0.0, 1.0, 67));
}

#[test]
fn a_line_of_words_is_sung_a_syllable_a_note() {
    let (mut s, _, _) = with_clip(
        "Voice",
        2.0,
        (0..6).map(|i| note(20 + i, i as f64, 1.0, 64)).collect(),
    );
    s.dispatch(Action::Score(ScoreOp::Lyrics {
        from: NoteId(20),
        text: "Hel-lo world _ a-gain".into(),
    }))
    .unwrap();
    let doc = s.score().unwrap();
    let e = &doc.sheet.parts[0].measures[0].events;
    let l = |i: usize| e[i].lyric.as_ref().map(|l| (l.text.clone(), l.syllabic));
    assert_eq!(l(0), Some(("Hel".into(), Syllabic::Begin)));
    assert_eq!(l(1), Some(("lo".into(), Syllabic::End)));
    assert_eq!(l(2).map(|x| x.0), Some("world".into()));
    assert!(e[2].lyric.as_ref().unwrap().extend && e[3].held);
    let next = &doc.sheet.parts[0].measures[1].events;
    assert_eq!(
        next[0]
            .lyric
            .as_ref()
            .map(|l| (l.text.as_str(), l.syllabic)),
        Some(("a", Syllabic::Begin))
    );
    let xml = faderframe_leadsheet::musicxml::write(&doc.sheet, "2026-11-04");
    assert!(xml.contains("<text>Hel</text>"));
}

#[test]
fn chord_symbols_go_to_the_chord_track() {
    let (mut s, _, _) = with_clip("Lead", 2.0, vec![note(1, 0.0, 4.0, 64)]);
    s.dispatch(Action::Score(ScoreOp::ChordSymbol {
        at: MusicalTime::ZERO,
        text: "Am7".into(),
    }))
    .unwrap();
    s.dispatch(Action::Score(ScoreOp::ChordSymbol {
        at: q(4.0),
        text: "D7".into(),
    }))
    .unwrap();
    let chords = &s.project().chords;
    assert_eq!(chords.len(), 2);
    assert_eq!(chords[0].end, q(4.0));
    let doc = s.score().unwrap();
    assert_eq!(doc.sheet.parts[0].measures[0].chords[0].text(), "Am7");
    assert!(
        s.dispatch(Action::Score(ScoreOp::ChordSymbol {
            at: q(1.0),
            text: "nonsense".into(),
        }))
        .is_err()
    );
}

/// Display quantise is the clip's; quantising to the score moves the
/// played notes to where it writes them.
#[test]
fn played_notes_are_quantised_to_the_score_on_request() {
    let (mut s, _, c) = with_clip(
        "Lead",
        1.0,
        vec![
            note(1, 0.02, 0.45, 60),
            note(2, 0.51, 0.47, 62),
            note(3, 1.03, 0.93, 64),
        ],
    );
    let doc = s.score().unwrap();
    let bar = &doc.sheet.parts[0].measures[0];
    assert_eq!(bar.events[0].duration.value, Value::Eighth);
    // The MIDI kept its timing.
    assert_eq!(notes(&s, c)[0].0, 0.02);
    s.dispatch(Action::Score(ScoreOp::Quantize(Vec::new())))
        .unwrap();
    assert_eq!(
        notes(&s, c),
        vec![(0.0, 0.5, 60), (0.5, 0.5, 62), (1.0, 1.0, 64)]
    );
    s.dispatch(Action::Score(ScoreOp::Display {
        clips: vec![c],
        grid: ScoreGrid::Triplets,
        legato: false,
    }))
    .unwrap();
    let n = s.project().clip_notation(c);
    assert_eq!((n.grid, n.legato), (ScoreGrid::Triplets, false));
}

/// The piano roll and the score are one model: an edit in either is the
/// other's.
#[test]
fn the_piano_roll_and_the_score_edit_the_same_notes() {
    let (mut s, _, c) = with_clip(
        "Lead",
        1.0,
        vec![note(1, 0.0, 1.0, 60), note(2, 1.0, 1.0, 62)],
    );
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![NoteId(2)],
        op: NoteOp::Transpose(3),
    })
    .unwrap();
    let doc = s.score().unwrap();
    let p = doc.sheet.parts[0].measures[0].events[1].pitch.unwrap();
    assert_eq!(p.midi, 65);
    s.dispatch(Action::Score(ScoreOp::Transpose {
        notes: vec![NoteId(1)],
        by: Interval::Octaves(1),
    }))
    .unwrap();
    assert_eq!(notes(&s, c)[0].2, 72);
    // Deleted in the score: gone from the clip, one step.
    s.dispatch(Action::Score(ScoreOp::Delete(vec![NoteId(2)])))
        .unwrap();
    assert_eq!(notes(&s, c).len(), 1);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(notes(&s, c).len(), 2);
}

/// The pointer enters a note at a time on a staff place: its letter with
/// the key's accidental.
#[test]
fn a_note_is_entered_where_it_is_pointed_at() {
    let (mut s, _, c) = with_clip("Lead", 1.0, Vec::new());
    entering(&mut s);
    // G4 is diatonic 4 + 7 × 4.
    s.dispatch(Action::Score(ScoreOp::EnterAt {
        part: 0,
        at: q(2.0),
        diatonic: 4 + 28,
    }))
    .unwrap();
    assert_eq!(notes(&s, c), vec![(2.0, 1.0, 67)]);
    assert_eq!(s.score_cursor().unwrap().at, q(3.0));
}

/// Several tracks are a staff each, a keyboard's on a grand staff.
#[test]
fn tracks_are_staves_and_a_piano_a_grand_staff() {
    let (mut s, lead, _) = with_clip("Lead", 1.0, vec![note(1, 0.0, 4.0, 72)]);
    let piano = s.add_track(TrackKind::Instrument).unwrap();
    s.edit(Command::RenameTrack {
        track: piano,
        name: "Piano".into(),
    })
    .unwrap();
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(80_002),
            track: piano,
            name: "chords".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: q(4.0),
                notes: vec![
                    note(5, 0.0, 4.0, 48),
                    note(6, 0.0, 4.0, 64),
                    note(7, 0.0, 4.0, 67),
                ],
                ..MidiClip::default()
            }),
        }),
    })
    .unwrap();
    s.dispatch(Action::Score(ScoreOp::Show(ScoreOf::Tracks(vec![
        lead, piano,
    ]))))
    .unwrap();
    let doc = s.score().unwrap();
    assert_eq!(doc.parts.len(), 3);
    assert!(doc.sheet.parts[1].brace);
    assert!(doc.parts[2].lower);
    assert_eq!(doc.part_of(NoteId(5)), Some(2));
    assert_eq!(doc.part_of(NoteId(7)), Some(1));
}

/// The score is made again only when what it reads changes.
#[test]
fn the_score_is_kept_until_its_notes_change() {
    let (mut s, t, _) = with_clip("Lead", 1.0, vec![note(1, 0.0, 1.0, 60)]);
    let a = s.score().unwrap();
    s.edit(Command::SetTrackVolume { track: t, db: -6.0 })
        .unwrap();
    let b = s.score().unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    s.score_keys("n c").unwrap();
    let c = s.score().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a, &c));
}

#[test]
fn step_input_enters_the_scores_value() {
    let (mut s, _, c) = with_clip("Lead", 1.0, Vec::new());
    s.dispatch(Action::SetStepInput(Some(StepInput {
        clip: c,
        cursor: MusicalTime::ZERO,
        step: q(1.0),
    })))
    .unwrap();
    s.score_keys("8").unwrap();
    assert_eq!(s.step_input().unwrap().step, q(0.5));
}

#[test]
fn the_score_is_exported_as_pdf_and_musicxml() {
    let (mut s, _, _) = with_clip("Lead", 1.0, vec![note(1, 0.0, 1.0, 60)]);
    let dir = std::env::temp_dir().join(format!("ff-score-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("lead.pdf");
    let xml = dir.join("lead.musicxml");
    s.dispatch(Action::Score(ScoreOp::Export(pdf.clone())))
        .unwrap();
    s.dispatch(Action::Score(ScoreOp::Export(xml.clone())))
        .unwrap();
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    let text = std::fs::read_to_string(&xml).unwrap();
    assert!(text.contains("<step>C</step>"));
    let _ = std::fs::remove_dir_all(&dir);
    // A strip is one system.
    let settings = ScoreSettings {
        layout: ScoreLayout::Strip,
        ..s.score_settings()
    };
    s.dispatch(Action::Score(ScoreOp::Settings(settings)))
        .unwrap();
    assert_eq!(s.score().unwrap().map.systems.len(), 1);
    let _ = ScoreKey::Escape;
}
