#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_midi::theory::{Quality, Scale};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// A song in G with a pickup: ties across the bar, a chord, a triplet, a
/// second voice after a backup, a rest, words (a melisma), chord symbols,
/// a tempo by metronome then by sound, a change to 3/4 and to D minor.
const SONG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 4.0 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">
<score-partwise version="4.0">
  <work><work-title>Test Song</work-title></work>
  <identification><creator type="composer">Someone</creator></identification>
  <part-list>
    <score-part id="P1"><part-name>Voice</part-name>
      <score-instrument id="P1-I1"><instrument-name>Voice</instrument-name></score-instrument>
      <midi-instrument id="P1-I1"><midi-channel>2</midi-channel><midi-program>53</midi-program></midi-instrument>
    </score-part>
  </part-list>
  <part id="P1">
    <measure number="0" implicit="yes">
      <attributes><divisions>12</divisions><key><fifths>1</fifths><mode>major</mode></key>
        <time><beats>4</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <direction placement="above"><direction-type><metronome><beat-unit>half</beat-unit><per-minute>50</per-minute></metronome></direction-type></direction>
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>single</syllabic><text>Oh</text></lyric></note>
    </measure>
    <measure number="1">
      <harmony><root><root-step>G</root-step></root><kind text="">major</kind></harmony>
      <note><pitch><step>G</step><octave>4</octave></pitch><duration>24</duration><voice>1</voice><type>half</type>
        <lyric number="1"><syllabic>begin</syllabic><text>hap</text></lyric></note>
      <note><pitch><step>A</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>end</syllabic><text>py</text><extend/></lyric></note>
      <note><chord/><pitch><step>C</step><octave>5</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type></note>
      <harmony><root><root-step>E</root-step></root><kind text="m7">minor-seventh</kind><offset>4</offset></harmony>
      <note><pitch><step>B</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
      <note><pitch><step>D</step><octave>5</octave></pitch><duration>4</duration><tie type="start"/><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>
        <lyric number="1"><syllabic>single</syllabic><text>day</text><end-line/></lyric></note>
      <backup><duration>48</duration></backup>
      <note><pitch><step>G</step><octave>3</octave></pitch><duration>48</duration><voice>2</voice><type>whole</type></note>
    </measure>
    <measure number="2">
      <attributes><time><beats>3</beats><beat-type>4</beat-type></time><key><fifths>-1</fifths><mode>minor</mode></key></attributes>
      <sound tempo="120"/>
      <harmony><root><root-step>B</root-step><root-alter>-1</root-alter></root><kind>major</kind><bass><bass-step>D</bass-step></bass></harmony>
      <note><pitch><step>D</step><octave>5</octave></pitch><duration>12</duration><tie type="stop"/><voice>1</voice><type>quarter</type></note>
      <note><rest/><duration>12</duration><voice>1</voice><type>quarter</type></note>
      <note><pitch><step>B</step><alter>-1</alter><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>single</syllabic><text>Now</text></lyric></note>
    </measure>
  </part>
</score-partwise>
"#;

#[test]
fn a_song_reads_as_it_sounds() {
    let s = read_xml(SONG).unwrap();
    assert_eq!(s.title, "Test Song");
    assert_eq!(s.composer, "Someone");
    assert!(s.notes.is_empty(), "{:?}", s.notes);
    // The pickup sits late in a whole bar.
    assert_eq!(s.measures.len(), 3);
    assert!(close(s.measures[0].start, 3.0));
    assert!(close(s.measures[1].start, 4.0));
    assert!(close(s.measures[2].start, 8.0));
    assert_eq!(s.measures[0].time, (4, 4));
    assert_eq!(s.measures[2].time, (3, 4));
    assert_eq!(s.meter_changes(), vec![(0, (4, 4)), (2, (3, 4))]);
    assert!(close(s.length(), 11.0));
    // Half = 50 is quarter = 100; then 120.
    assert_eq!(s.tempos.len(), 2);
    assert!(close(s.tempos[0].at, 3.0) && close(s.tempos[0].bpm, 100.0));
    assert!(close(s.tempos[1].at, 8.0) && close(s.tempos[1].bpm, 120.0));
    assert_eq!(s.keys.len(), 2);
    assert_eq!(s.keys[0].key, Key::new(7, Scale::Major));
    assert_eq!(s.keys[1].key, Key::new(2, Scale::Minor));
    let p = &s.parts[0];
    assert_eq!(p.name, "Voice");
    assert_eq!((p.channel, p.program), (Some(1), Some(52)));
    let notes: Vec<(f64, f64, u8, u8)> = p
        .notes
        .iter()
        .map(|n| (n.start, n.length, n.key, n.voice))
        .collect();
    let expect = [
        (3.0, 1.0, 62, 1),
        (4.0, 2.0, 67, 1),
        (4.0, 4.0, 55, 2),
        (6.0, 1.0, 69, 1),
        (6.0, 1.0, 72, 1),
        (7.0, 1.0 / 3.0, 71, 1),
        (7.0 + 1.0 / 3.0, 1.0 / 3.0, 72, 1),
        // The last of the triplet tied over the bar line to a quarter.
        (7.0 + 2.0 / 3.0, 1.0 / 3.0 + 1.0, 74, 1),
        (10.0, 1.0, 70, 1),
    ];
    assert_eq!(notes.len(), expect.len(), "{notes:?}");
    for (got, want) in notes.iter().zip(&expect) {
        assert!(
            close(got.0, want.0) && close(got.1, want.1) && got.2 == want.2 && got.3 == want.3,
            "{got:?} vs {want:?}"
        );
    }
    assert!(p.notes.iter().all(|n| n.velocity == DEFAULT_VELOCITY));
    assert!(p.notes.iter().all(|n| n.channel == Some(1)));
    // Chords: G, Em7 a third into beat 3 (offset 4/12), B♭/D.
    let chords: Vec<(f64, Chord)> = p.chords.iter().map(|c| (c.at, c.chord.unwrap())).collect();
    assert!(close(chords[0].0, 4.0));
    assert_eq!(chords[0].1, Chord::new(7, Quality::Major));
    assert!(close(chords[1].0, 7.0 + 1.0 / 3.0), "{chords:?}");
    assert_eq!(chords[1].1, Chord::new(4, Quality::Minor7));
    assert_eq!(
        chords[2].1,
        Chord::new(10, Quality::Major).with_bass(Some(2))
    );
    let spans = s.chord_spans();
    assert!(close(spans[2].1, 11.0), "the last chord lasts to the end");
    // Words: "Oh happy day" ends its line; "py" is held over the next
    // notes.
    let lines = p.lyric_lines(&s.measures);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0].text, "Oh happy day");
    assert!(
        close(lines[0].start, 3.0) && close(lines[0].end, 9.0),
        "{lines:?}"
    );
    assert_eq!(lines[1].text, "Now");
    let py = p.lyrics.iter().find(|l| l.text == "py").unwrap();
    assert!(
        py.extend && close(py.end, 7.0 + 2.0 / 3.0),
        "held over the triplet: {py:?}"
    );
}

const DYNAMICS: &str = r#"<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>1</divisions><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
      <direction><direction-type><dynamics><p/></dynamics></direction-type></direction>
      <note><pitch><step>C</step><octave>4</octave></pitch><duration>1</duration></note>
      <direction><direction-type><wedge type="crescendo"/></direction-type></direction>
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>1</duration></note>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>1</duration></note>
      <direction><direction-type><wedge type="stop"/></direction-type></direction>
      <direction><direction-type><dynamics><f/></dynamics></direction-type></direction>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>1</duration>
        <notations><articulations><staccato/></articulations></notations></note>
    </measure>
    <measure number="2">
      <note><pitch><step>G</step><octave>4</octave></pitch><duration>1</duration>
        <notations><dynamics><sfz/></dynamics></notations></note>
      <note><pitch><step>A</step><octave>4</octave></pitch><duration>1</duration>
        <notations><articulations><accent/></articulations></notations></note>
      <note dynamics="50"><pitch><step>B</step><octave>4</octave></pitch><duration>1</duration></note>
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>1</duration><grace/></note>
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>1</duration>
        <notations><articulations><staccatissimo/><strong-accent/></articulations></notations></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn dynamics_and_articulations_shape_velocities_and_lengths() {
    let s = read_xml(DYNAMICS).unwrap();
    let n = &s.parts[0].notes;
    let v: Vec<u8> = n.iter().map(|n| n.velocity).collect();
    // p, the wedge from p (49) up to f (96) over two beats, f, sfz on one
    // note, f + accent, 45 (50 % of 90), f + marcato.
    assert_eq!(v[0], 49);
    assert_eq!(v[1], 49);
    assert!(v[2] > 49 && v[2] < 96, "{v:?}");
    assert_eq!(v[3], 96);
    assert_eq!(v[4], 112);
    assert_eq!(v[5], 112);
    assert_eq!(v[6], 45);
    assert_eq!(v[7], 120);
    assert!(close(n[3].length, 0.5), "staccato");
    assert!(close(n[7].length, 0.25), "staccatissimo wins over marcato");
    assert!(close(n[0].length, 1.0));
    assert_eq!(n.len(), 8, "the grace note is left out");
    assert!(s.notes.iter().any(|t| t.contains("grace")), "{:?}", s.notes);
}

/// Bars 1–2 repeated with first and second endings (bar 3 the first,
/// bar 4 the second); verse 2's words on the second pass; bar 5 after.
const REPEATS: &str = r#"<score-partwise version="4.0">
  <part-list><score-part id="P1"><part-name>Tune</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <barline location="left"><repeat direction="forward"/></barline>
      <attributes><divisions>1</divisions><time><beats>2</beats><beat-type>4</beat-type></time></attributes>
      <note><pitch><step>C</step><octave>4</octave></pitch><duration>2</duration>
        <lyric number="1"><text>one</text></lyric><lyric number="2"><text>three</text></lyric></note>
    </measure>
    <measure number="2">
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>2</duration>
        <lyric number="1"><text>two</text></lyric></note>
    </measure>
    <measure number="3">
      <barline location="left"><ending number="1" type="start"/></barline>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>2</duration></note>
      <barline location="right"><ending number="1" type="stop"/><repeat direction="backward"/></barline>
    </measure>
    <measure number="4">
      <barline location="left"><ending number="2" type="start"/></barline>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>2</duration></note>
      <barline location="right"><ending number="2" type="discontinue"/></barline>
    </measure>
    <measure number="5">
      <note><pitch><step>G</step><octave>4</octave></pitch><duration>2</duration></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn repeats_and_endings_are_played_out() {
    let s = read_xml(REPEATS).unwrap();
    let numbers: Vec<&str> = s.measures.iter().map(|m| m.number.as_str()).collect();
    assert_eq!(numbers, ["1", "2", "3", "1", "2", "4", "5"]);
    let keys: Vec<u8> = s.parts[0].notes.iter().map(|n| n.key).collect();
    assert_eq!(keys, [60, 62, 64, 60, 62, 65, 67]);
    assert!(close(s.parts[0].notes[3].start, 6.0));
    let words: Vec<&str> = s.parts[0].lyrics.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        words,
        ["one", "two", "three", "two"],
        "verse 2 on the second pass"
    );
}

/// A piano on a grand staff: right hand, backup, left hand on staff 2.
const PIANO: &str = r#"<score-partwise version="4.0">
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>2</divisions><time><beats>2</beats><beat-type>4</beat-type></time><staves>2</staves>
        <clef number="1"><sign>G</sign><line>2</line></clef><clef number="2"><sign>F</sign><line>4</line></clef></attributes>
      <note><pitch><step>E</step><octave>5</octave></pitch><duration>2</duration><voice>1</voice><staff>1</staff></note>
      <note><pitch><step>D</step><octave>5</octave></pitch><duration>2</duration><voice>1</voice><staff>1</staff></note>
      <backup><duration>4</duration></backup>
      <note><pitch><step>C</step><octave>3</octave></pitch><duration>4</duration><voice>5</voice><staff>2</staff></note>
      <note><chord/><pitch><step>G</step><octave>3</octave></pitch><duration>4</duration><voice>5</voice><staff>2</staff></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn a_grand_staff_is_one_part() {
    let s = read_xml(PIANO).unwrap();
    assert_eq!(s.parts.len(), 1);
    let p = &s.parts[0];
    assert_eq!(p.staves, 2);
    let mut keys: Vec<(u8, u8)> = p.notes.iter().map(|n| (n.key, n.staff)).collect();
    keys.sort_unstable();
    assert_eq!(keys, [(48, 2), (55, 2), (74, 1), (76, 1)]);
    assert!(
        p.notes
            .iter()
            .filter(|n| n.staff == 2)
            .all(|n| close(n.start, 0.0) && close(n.length, 2.0))
    );
}

#[test]
fn timewise_scores_read_like_partwise_ones() {
    let timewise = r#"<score-timewise version="4.0">
      <part-list><score-part id="A"><part-name>A</part-name></score-part>
        <score-part id="B"><part-name>B</part-name>
          <score-instrument id="B-I1"><instrument-name>Kick</instrument-name></score-instrument>
          <midi-instrument id="B-I1"><midi-channel>10</midi-channel><midi-unpitched>37</midi-unpitched></midi-instrument>
        </score-part></part-list>
      <measure number="1">
        <part id="A"><attributes><divisions>1</divisions><time><beats>2</beats><beat-type>4</beat-type></time>
          <transpose><diatonic>-1</diatonic><chromatic>-2</chromatic></transpose></attributes>
          <note><pitch><step>D</step><octave>4</octave></pitch><duration>2</duration></note></part>
        <part id="B"><attributes><divisions>1</divisions></attributes>
          <note><unpitched><display-step>F</display-step><display-octave>4</display-octave></unpitched><duration>1</duration><instrument id="B-I1"/></note>
          <note><rest/><duration>1</duration></note></part>
      </measure>
      <measure number="2">
        <part id="A"><note><pitch><step>E</step><octave>4</octave></pitch><duration>2</duration></note></part>
        <part id="B"><note><rest/><duration>2</duration></note></part>
      </measure>
    </score-timewise>"#;
    let s = read_xml(timewise).unwrap();
    assert_eq!(s.parts.len(), 2);
    // A B-flat instrument: written D sounds C.
    let a: Vec<u8> = s.parts[0].notes.iter().map(|n| n.key).collect();
    assert_eq!(a, [60, 62]);
    let b = &s.parts[1];
    assert!(b.percussion);
    assert_eq!((b.notes[0].key, b.notes[0].channel), (36, Some(9)));
    assert_eq!(s.measures.len(), 2);
}

#[test]
fn compressed_scores_are_read_through_their_container() {
    use crate::zip::ZipWriter;
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()), "t.mxl");
    w.add("mimetype", b"application/vnd.recordare.musicxml", false)
        .unwrap();
    w.add(
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?><container><rootfiles>
            <rootfile full-path="scores/song.musicxml" media-type="application/vnd.recordare.musicxml+xml"/>
            </rootfiles></container>"#,
        true,
    )
    .unwrap();
    w.add("scores/song.musicxml", SONG.as_bytes(), true)
        .unwrap();
    let bytes = w.finish().unwrap().into_inner();
    let s = read_bytes(&bytes).unwrap();
    assert_eq!(s.title, "Test Song");
    assert_eq!(s.parts[0].notes.len(), 9);
    // UTF-16 text (Finale writes it) reads too.
    let mut utf16 = vec![0xFF, 0xFE];
    for u in DYNAMICS.encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(read_bytes(&utf16).unwrap().parts[0].notes.len(), 8);
    assert!(read_xml("<html/>").is_err());
}

#[test]
fn odd_bars_get_signatures_of_their_own() {
    let x = r#"<score-partwise version="4.0">
      <part-list><score-part id="P1"><part-name>X</part-name></score-part></part-list>
      <part id="P1">
        <measure number="1"><attributes><divisions>2</divisions><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
          <note><pitch><step>C</step><octave>4</octave></pitch><duration>8</duration></note></measure>
        <measure number="2"><note><pitch><step>C</step><octave>4</octave></pitch><duration>5</duration></note></measure>
        <measure number="3"><note><pitch><step>C</step><octave>4</octave></pitch><duration>8</duration></note></measure>
        <measure number="4"><attributes><time><beats>3+2</beats><beat-type>8</beat-type></time></attributes>
          <note><pitch><step>C</step><octave>4</octave></pitch><duration>5</duration></note></measure>
      </part></score-partwise>"#;
    let s = read_xml(x).unwrap();
    let t: Vec<(u8, u8)> = s.measures.iter().map(|m| m.time).collect();
    assert_eq!(t, [(4, 4), (5, 8), (4, 4), (5, 8)]);
    assert!(close(s.measures[2].start, 6.5));
}

/// What the lead-sheet writer writes reads back: notes (as the sheet
/// quantised them), chords, key, metre, tempo and the words by line; a
/// piano part on its grand staff as one part.
#[test]
fn lead_sheets_read_back() {
    use faderframe_leadsheet::build::{
        Bar, ChordAt as SheetChord, Input, Line, Note as SheetNote, Staff,
    };
    use faderframe_leadsheet::score::Clef;
    let tune: Vec<SheetNote> = [
        (0.0, 1.0, 69),
        (1.0, 1.5, 71),
        (1.5, 2.0, 72),
        (2.0, 4.0, 76),
        (4.0, 5.0, 74),
        (5.0, 6.0, 72),
        (6.0, 6.5, 71),
        (6.5, 8.0, 69),
        (8.0, 9.0, 68),
        (9.0, 12.0, 69),
    ]
    .iter()
    .map(|&(start, end, key)| SheetNote { start, end, key })
    .collect();
    let mut piano = Vec::new();
    for b in 0..3 {
        for k in [48u8, 52, 55] {
            piano.push(SheetNote {
                start: 4.0 * f64::from(b),
                end: 4.0 * f64::from(b) + 4.0,
                key: k,
            });
        }
        for i in 0..4 {
            piano.push(SheetNote {
                start: 4.0 * f64::from(b) + f64::from(i),
                end: 4.0 * f64::from(b) + f64::from(i) + 1.0,
                key: [76u8, 79, 77, 74][i as usize],
            });
        }
    }
    let (right, left) = faderframe_leadsheet::build::hands(&piano);
    let sheet = faderframe_leadsheet::build(&Input {
        title: "Round Trip".into(),
        composer: "FaderFrame".into(),
        key: Some(Key::new(9, Scale::Minor)),
        bars: (0..3)
            .map(|i| Bar {
                start: f64::from(i) * 4.0,
                time: (4, 4),
            })
            .collect(),
        tempo: Some(96.0),
        staves: vec![
            Staff {
                name: "Voice".into(),
                notes: tune.clone(),
                lines: vec![
                    Line {
                        start: 0.0,
                        end: 4.0,
                        text: "Sing a song".into(),
                    },
                    Line {
                        start: 4.0,
                        end: 12.0,
                        text: "of the night".into(),
                    },
                ],
                ..Staff::default()
            },
            Staff {
                name: "Piano".into(),
                notes: right,
                chords: true,
                clef: Some(Clef::Treble),
                brace: true,
                ..Staff::default()
            },
            Staff {
                notes: left,
                chords: true,
                clef: Some(Clef::Bass),
                ..Staff::default()
            },
        ],
        chords: vec![
            SheetChord {
                start: 0.0,
                chord: Chord::new(9, Quality::Minor7),
            },
            SheetChord {
                start: 4.0,
                chord: Chord::new(2, Quality::Minor),
            },
            SheetChord {
                start: 6.0,
                chord: Chord::new(4, Quality::Dominant7),
            },
            SheetChord {
                start: 8.0,
                chord: Chord::new(9, Quality::Minor).with_bass(Some(0)),
            },
        ],
        ..Input::default()
    });
    let xml = faderframe_leadsheet::musicxml::write(&sheet, "2026-11-03");
    let s = read_xml(&xml).unwrap();
    assert!(s.notes.is_empty(), "{:?}", s.notes);
    assert_eq!(s.title, "Round Trip");
    assert_eq!(s.parts.len(), 2, "the grand staff is one part");
    assert_eq!(s.measures.len(), 3);
    assert!(s.measures.iter().all(|m| m.time == (4, 4)));
    assert_eq!(s.keys[0].key, Key::new(9, Scale::Minor));
    assert!(close(s.tempos[0].bpm, 96.0));
    // The melody comes back note for note (on the grid it was on).
    let voice: Vec<(f64, f64, u8)> = s.parts[0]
        .notes
        .iter()
        .map(|n| (n.start, n.start + n.length, n.key))
        .collect();
    assert_eq!(voice.len(), tune.len(), "{voice:?}");
    for (got, want) in voice.iter().zip(&tune) {
        assert!(
            close(got.0, want.start) && close(got.1, want.end) && got.2 == want.key,
            "{got:?} vs {want:?}"
        );
    }
    // Both hands, every note.
    let mut back: Vec<(i64, i64, u8)> = s.parts[1]
        .notes
        .iter()
        .map(|n| {
            (
                (n.start * 1e6).round() as i64,
                ((n.start + n.length) * 1e6).round() as i64,
                n.key,
            )
        })
        .collect();
    let mut want: Vec<(i64, i64, u8)> = piano
        .iter()
        .map(|n| {
            (
                (n.start * 1e6).round() as i64,
                (n.end * 1e6).round() as i64,
                n.key,
            )
        })
        .collect();
    back.sort_unstable();
    want.sort_unstable();
    assert_eq!(back, want);
    assert_eq!(s.parts[1].staves, 2);
    // The chords.
    let chords = s.chord_spans();
    let got: Vec<(f64, Chord)> = chords.iter().map(|c| (c.0, c.2)).collect();
    assert_eq!(got.len(), 4);
    assert_eq!(got[1], (4.0, Chord::new(2, Quality::Minor)));
    assert_eq!(got[2], (6.0, Chord::new(4, Quality::Dominant7)));
    assert_eq!(got[3].1, Chord::new(9, Quality::Minor).with_bass(Some(0)));
    assert!(close(chords[3].1, 12.0));
    // The words, line by line.
    let lines = s.parts[0].lyric_lines(&s.measures);
    let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(text, ["Sing a song", "of the night"]);
    assert!(
        close(lines[0].start, 0.0) && close(lines[1].start, 4.0),
        "{lines:?}"
    );
    assert!(lines[1].end > 9.0, "{lines:?}");
}
