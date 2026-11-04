use super::*;
use crate::score::{Beam, Tuplet, Value};
use faderframe_midi::theory::Scale;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            start: i as f64 * 4.0,
            time: (4, 4),
        })
        .collect()
}

fn note(id: u64, start: f64, end: f64, key: u8) -> Written {
    Written::new(id, start, end, key)
}

fn score(notes: Vec<Written>, n: usize, key: Option<Key>) -> Notated {
    notate(&ScoreInput {
        title: "Test".into(),
        bars: bars(n),
        tempo: Some(120.0),
        key,
        staves: vec![ScoreStaff {
            name: "Keys".into(),
            notes,
            ..ScoreStaff::default()
        }],
        ..ScoreInput::default()
    })
}

/// (start, divisions, rest, tied on) of a voice's events.
fn values(events: &[Event]) -> Vec<(u32, u32, bool, bool)> {
    events
        .iter()
        .map(|e| (e.at, e.duration.divisions(), e.is_rest(), e.tie))
        .collect()
}

const C: Option<Key> = Some(Key {
    root: 0,
    scale: Scale::Major,
});

#[test]
fn quarter_notes_are_written_as_quarters_from_their_notes() {
    let notes = (0..4)
        .map(|i| note(10 + i, i as f64, i as f64 + 1.0, 60 + 2 * i as u8))
        .collect();
    let n = score(notes, 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert!(m.under.is_empty());
    assert_eq!(
        values(&m.events),
        vec![
            (0, 12, false, false),
            (12, 12, false, false),
            (24, 12, false, false),
            (36, 12, false, false)
        ]
    );
    let sources: Vec<&[u64]> = m.events.iter().map(|e| e.sources.as_slice()).collect();
    assert_eq!(sources, vec![&[10][..], &[11], &[12], &[13]]);
    assert_eq!(n.written.len(), 4);
    assert!(n.written.iter().all(|w| w.voice == 1 && w.part == 0));
}

/// A whole note held under four quarters: the quarters (the higher line)
/// are the first voice, stems up, the whole note the second — not one line
/// of chords cut where each quarter starts.
#[test]
fn a_held_note_under_a_moving_line_is_a_second_voice() {
    let mut notes = vec![note(1, 0.0, 4.0, 48)];
    notes.extend((0..4).map(|i| note(2 + i, i as f64, i as f64 + 1.0, 64 + i as u8)));
    let n = score(notes, 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(m.events.len(), 4, "{:?}", values(&m.events));
    assert!(m.events.iter().all(|e| e.duration.value == Value::Quarter));
    assert_eq!(m.under.len(), 1);
    assert_eq!(m.under[0].duration.value, Value::Whole);
    assert_eq!(m.under[0].sources, vec![1]);
    // The other way round: a held high note over a moving bass.
    let mut notes = vec![note(1, 0.0, 4.0, 76)];
    notes.extend((0..4).map(|i| note(2 + i, i as f64, i as f64 + 1.0, 48 + i as u8)));
    let n = score(notes, 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(m.events.len(), 1);
    assert_eq!(m.events[0].sources, vec![1]);
    assert_eq!(m.under.len(), 4);
}

/// Legato playing overlaps each note into the next a little: one line,
/// not two voices.
#[test]
fn legato_overlaps_stay_one_voice() {
    let notes = (0..8)
        .map(|i| {
            let s = i as f64 * 0.5;
            note(i, s + 0.01, s + 0.5 + 0.06, 60 + (i % 5) as u8)
        })
        .collect();
    let n = score(notes, 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert!(m.under.is_empty());
    assert_eq!(m.events.len(), 8);
    assert!(m.events.iter().all(|e| e.duration.value == Value::Eighth));
    // Beamed in pairs by the beat.
    assert_eq!(m.events[0].beams[0], Some(Beam::Begin));
    assert_eq!(m.events[1].beams[0], Some(Beam::End));
}

#[test]
fn a_note_across_the_bar_line_is_tied_and_both_halves_select_it() {
    let n = score(vec![note(7, 3.0, 5.0, 67)], 2, C);
    let p = &n.sheet.parts[0];
    let last = p.measures[0].events.last().unwrap();
    let first = &p.measures[1].events[0];
    assert!(last.tie && !last.tied_from);
    assert!(first.tied_from);
    assert_eq!(last.sources, vec![7]);
    assert_eq!(first.sources, vec![7]);
    assert_eq!(last.duration.value, Value::Quarter);
    assert_eq!(first.duration.value, Value::Quarter);
}

/// Accidentals by the key: in G an F♯ needs none, an F needs a natural,
/// and the F♯ after it in the same bar its sharp again.
#[test]
fn accidentals_follow_the_key_and_the_bar() {
    let g = Some(Key::new(7, Scale::Major));
    let notes = vec![
        note(1, 0.0, 1.0, 66),
        note(2, 1.0, 2.0, 65),
        note(3, 2.0, 3.0, 66),
        note(4, 4.0, 5.0, 66),
    ];
    let n = score(notes, 2, g);
    assert_eq!(n.sheet.key.fifths, 1);
    let accidentals: Vec<Option<i8>> = n.sheet.parts[0]
        .measures
        .iter()
        .flat_map(|m| m.events.iter())
        .filter_map(|e| e.pitch.map(|p| p.accidental))
        .collect();
    assert_eq!(accidentals, vec![None, Some(0), Some(1), None]);
}

#[test]
fn a_note_is_spelt_on_the_letter_it_was_given() {
    let mut flat = note(1, 0.0, 4.0, 61);
    flat.step = Some(1);
    let n = score(vec![flat], 1, C);
    let p = n.sheet.parts[0].measures[0].events[0].pitch.unwrap();
    assert_eq!((p.step, p.alter, p.octave), (1, -1, 4));
    assert_eq!(p.accidental, Some(-1));
    // B♯3 is middle C.
    assert_eq!(spell_on(60, Some(6), KeySig::C), (6, 1, 3));
    // A letter too far away: as the key spells it.
    assert_eq!(spell_on(60, Some(2), KeySig::C), (0, 0, 4));
    assert_eq!(spellings(61, KeySig::C), vec![(0, 1), (1, -1), (6, 2)]);
}

/// A staccato quarter is played as an eighth: it is still written as a
/// quarter (with its dot over it), not an eighth and a rest.
#[test]
fn a_staccato_note_reads_at_its_written_value() {
    let mut a = note(1, 0.0, 0.5, 67);
    a.marks.staccato = true;
    let n = score(vec![a, note(2, 1.0, 2.0, 67)], 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(m.events[0].duration.value, Value::Quarter);
    assert!(m.events[0].marks.staccato);
    assert_eq!(m.events[1].at, 12);
}

/// A rest written on the grid stays (a note entered as an eighth with an
/// eighth's rest after it); a played note's short gap closes.
#[test]
fn written_rests_stay_and_played_gaps_close() {
    let n = score(vec![note(1, 0.0, 0.75, 60), note(2, 1.0, 2.0, 62)], 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(
        values(&m.events)[..2],
        [(0, 9, false, false), (9, 3, true, false)]
    );
    // Played: a sixteenth-grid beat whose first note ends off the grid,
    // a sixteenth short of the next — read as legato (a dotted eighth).
    let played = || {
        vec![
            note(1, 0.0, 0.4, 60),
            note(2, 0.75, 1.0, 62),
            note(3, 1.0, 2.0, 64),
        ]
    };
    let n = score(played(), 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(values(&m.events)[0], (0, 9, false, false));
    // Unless the score is told not to: an eighth and a sixteenth's rest.
    let n = notate(&ScoreInput {
        bars: bars(1),
        key: C,
        staves: vec![ScoreStaff {
            notes: played(),
            display: vec![(
                0.0,
                4.0,
                Display {
                    grid: Grid::Auto,
                    legato: false,
                },
            )],
            ..ScoreStaff::default()
        }],
        ..ScoreInput::default()
    });
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(
        values(&m.events)[..2],
        [(0, 6, false, false), (6, 3, true, false)]
    );
}

#[test]
fn triplets_are_written_as_triplets() {
    let third = 1.0 / 3.0;
    let notes = (0..3)
        .map(|i| note(i, i as f64 * third, (i + 1) as f64 * third, 60 + i as u8))
        .chain([note(9, 1.0, 2.0, 64)])
        .collect();
    let n = score(notes, 1, C);
    let m = &n.sheet.parts[0].measures[0];
    let first: Vec<(Value, bool, Option<Tuplet>)> = m.events[..3]
        .iter()
        .map(|e| (e.duration.value, e.duration.triplet, e.tuplet))
        .collect();
    assert_eq!(
        first,
        vec![
            (Value::Eighth, true, Some(Tuplet::Start)),
            (Value::Eighth, true, Some(Tuplet::Middle)),
            (Value::Eighth, true, Some(Tuplet::Stop)),
        ]
    );
}

#[test]
fn syllables_are_joined_into_words() {
    let mut a = note(1, 0.0, 1.0, 60);
    a.lyric = Some("Hel-".into());
    let mut b = note(2, 1.0, 2.0, 62);
    b.lyric = Some("lo_".into());
    let c = note(3, 2.0, 3.0, 64);
    let mut d = note(4, 3.0, 4.0, 65);
    d.lyric = Some("world".into());
    let n = score(vec![a, b, c, d], 1, C);
    let e = &n.sheet.parts[0].measures[0].events;
    let l = |i: usize| e[i].lyric.clone().unwrap();
    assert_eq!(
        (l(0).text.as_str(), l(0).syllabic),
        ("Hel", Syllabic::Begin)
    );
    assert_eq!((l(1).text.as_str(), l(1).syllabic), ("lo", Syllabic::End));
    assert!(l(1).extend && e[2].held && e[2].lyric.is_none());
    assert_eq!(l(3).syllabic, Syllabic::Single);
}

#[test]
fn a_note_given_a_voice_keeps_it_and_quantising_gives_its_place() {
    let mut low = note(1, 0.0, 2.0, 55);
    low.voice = Some(2);
    let n = score(vec![low, note(2, 0.02, 1.97, 67)], 1, C);
    let m = &n.sheet.parts[0].measures[0];
    assert_eq!(m.under[0].sources, vec![1]);
    assert_eq!(m.events[0].sources, vec![2]);
    let w = n.written.iter().find(|w| w.id == 2).unwrap();
    assert_eq!((w.start, w.end, w.voice), (0.0, 2.0, 1));
}

#[test]
fn notes_starting_and_ending_together_are_one_chord() {
    let n = score(
        vec![
            note(1, 0.0, 2.0, 60),
            note(2, 0.0, 2.0, 64),
            note(3, 0.0, 2.0, 67),
        ],
        1,
        C,
    );
    let m = &n.sheet.parts[0].measures[0];
    assert!(m.under.is_empty());
    assert_eq!(m.events[0].chord.len(), 2);
    assert_eq!(m.events[0].sources, vec![1, 2, 3]);
    assert_eq!(m.events[0].duration.value, Value::Half);
}

/// The marks and the dynamic go to the first piece of a tied note only.
#[test]
fn marks_and_dynamics_sit_on_the_note_they_were_given() {
    let mut a = note(1, 3.0, 5.0, 67);
    a.marks.accent = true;
    a.dynamic = Some(Dynamic::Mf);
    let n = score(vec![a], 2, C);
    let p = &n.sheet.parts[0];
    let first = p.measures[0].events.last().unwrap();
    let second = &p.measures[1].events[0];
    assert!(first.marks.accent && first.dynamic == Some(Dynamic::Mf));
    assert!(second.marks.is_empty() && second.dynamic.is_none());
    let xml = crate::musicxml::write(&n.sheet, "2026-11-04");
    assert!(xml.contains("<articulations><accent/></articulations>"));
    assert!(xml.contains("<dynamics><mf/></dynamics>"));
}

#[test]
fn a_second_voice_is_written_after_a_backup() {
    let mut notes = vec![note(1, 0.0, 4.0, 48)];
    notes.extend((0..4).map(|i| note(2 + i, i as f64, i as f64 + 1.0, 64 + i as u8)));
    let n = score(notes, 1, C);
    let xml = crate::musicxml::write(&n.sheet, "2026-11-04");
    assert!(xml.contains("<backup><duration>48</duration></backup>"));
    assert!(xml.contains("<voice>2</voice>"));
}

/// Three quarter triplets fill two beats: written as quarters under one
/// bracket, not as eighth triplets tied across the beat.
#[test]
fn quarter_triplets_fill_two_beats() {
    let t = 2.0 / 3.0;
    let notes = (0..3)
        .map(|i| note(i, i as f64 * t, (i + 1) as f64 * t, 60 + 2 * i as u8))
        .chain([note(9, 2.0, 4.0, 67)])
        .collect();
    let n = score(notes, 1, C);
    let m = &n.sheet.parts[0].measures[0];
    let got: Vec<(Value, bool, Option<Tuplet>, bool)> = m
        .events
        .iter()
        .map(|e| (e.duration.value, e.duration.triplet, e.tuplet, e.tie))
        .collect();
    assert_eq!(
        got,
        vec![
            (Value::Quarter, true, Some(Tuplet::Start), false),
            (Value::Quarter, true, Some(Tuplet::Middle), false),
            (Value::Quarter, true, Some(Tuplet::Stop), false),
            (Value::Half, false, None, false),
        ]
    );
}
