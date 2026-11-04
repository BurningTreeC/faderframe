#![allow(clippy::unwrap_used)]
//! MusicXML scores into a project: a part a track, tempo and metre (a
//! pickup bar kept on its beats), keys, chord symbols and words — one
//! undo step; a lead sheet FaderFrame wrote comes back.

use faderframe_engine::EngineConfig;
use faderframe_interchange::zip::ZipWriter;
use faderframe_midi::theory::{Chord, Key, Quality, Scale};
use faderframe_project::{MidiController, Project, TrackKind};
use faderframe_session::{Action, Session};
use faderframe_timeline::{MusicalTime, TimeSignature};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-musicxml-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn empty() -> Session {
    Session::new(Project::new("Empty", 48_000), None, EngineConfig::default()).unwrap()
}

fn q(x: f64) -> MusicalTime {
    MusicalTime::from_quarters(x)
}

/// A waltz in F with a pickup: a sung part with words and chords, a
/// piano on a grand staff with its MIDI program, a key change to D minor.
const WALTZ: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="4.0">
  <work><work-title>Waltz</work-title></work>
  <part-list>
    <score-part id="P1"><part-name>Voice</part-name></score-part>
    <score-part id="P2"><part-name>Piano</part-name>
      <score-instrument id="P2-I1"><instrument-name>Piano</instrument-name></score-instrument>
      <midi-instrument id="P2-I1"><midi-channel>2</midi-channel><midi-program>1</midi-program></midi-instrument>
    </score-part>
  </part-list>
  <part id="P1">
    <measure number="0" implicit="yes">
      <attributes><divisions>2</divisions><key><fifths>-1</fifths></key><time><beats>3</beats><beat-type>4</beat-type></time></attributes>
      <direction><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>90</per-minute></metronome></direction-type><sound tempo="90"/></direction>
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>2</duration><lyric><syllabic>single</syllabic><text>Come</text></lyric></note>
    </measure>
    <measure number="1">
      <harmony><root><root-step>F</root-step></root><kind>major</kind></harmony>
      <note><pitch><step>A</step><octave>4</octave></pitch><duration>4</duration><lyric><syllabic>begin</syllabic><text>dan</text></lyric></note>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>2</duration><lyric><syllabic>end</syllabic><text>cing</text></lyric></note>
    </measure>
    <measure number="2">
      <attributes><key><fifths>-1</fifths><mode>minor</mode></key></attributes>
      <harmony><root><root-step>D</root-step></root><kind>minor</kind></harmony>
      <note><pitch><step>D</step><octave>5</octave></pitch><duration>6</duration><lyric><syllabic>single</syllabic><text>slow</text><end-line/></lyric></note>
    </measure>
  </part>
  <part id="P2">
    <measure number="0" implicit="yes">
      <attributes><divisions>2</divisions><staves>2</staves></attributes>
      <note><rest/><duration>2</duration><staff>1</staff></note>
      <backup><duration>2</duration></backup>
      <note><rest/><duration>2</duration><staff>2</staff></note>
    </measure>
    <measure number="1">
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>6</duration><staff>1</staff></note>
      <backup><duration>6</duration></backup>
      <note><pitch><step>F</step><octave>3</octave></pitch><duration>6</duration><staff>2</staff></note>
      <note><chord/><pitch><step>C</step><octave>4</octave></pitch><duration>6</duration><staff>2</staff></note>
    </measure>
    <measure number="2">
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>6</duration><staff>1</staff></note>
      <backup><duration>6</duration></backup>
      <note><pitch><step>D</step><octave>3</octave></pitch><duration>6</duration><staff>2</staff></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn a_score_becomes_tracks_tempo_keys_chords_and_words() {
    let dir = tmp("waltz");
    let path = dir.join("Waltz.musicxml");
    std::fs::write(&path, WALTZ).unwrap();
    let mut s = empty();
    let tracks_before = s.project().tracks.len();
    s.dispatch(Action::ImportMusicXml {
        path: path.clone(),
        at: MusicalTime::ZERO,
        tempo: true,
    })
    .unwrap();
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks_before + 2);
    let voice = p.tracks.iter().find(|t| t.name == "Voice").unwrap();
    let piano = p.tracks.iter().find(|t| t.name == "Piano").unwrap();
    assert_eq!(
        (voice.kind, piano.kind),
        (TrackKind::Instrument, TrackKind::Instrument)
    );
    // 90 BPM in 3/4; the pickup on the last beat of a whole bar.
    assert!((p.timeline.tempo.points()[0].bpm - 90.0).abs() < 1e-9);
    assert_eq!(
        p.timeline.meter.signature_at(MusicalTime::ZERO),
        TimeSignature::new(3, 4).unwrap()
    );
    let v = p.clips_of(voice.id)[0].as_midi().unwrap();
    let notes: Vec<(f64, f64, u8)> = v
        .notes
        .iter()
        .map(|n| (n.start.quarters(), n.length.quarters(), n.key))
        .collect();
    assert_eq!(
        notes,
        [
            (2.0, 1.0, 72),
            (3.0, 2.0, 69),
            (5.0, 1.0, 65),
            (6.0, 3.0, 74)
        ]
    );
    assert_eq!(v.length, q(9.0));
    // The piano: both hands in one clip, its program at the start.
    let pc = p.clips_of(piano.id)[0].as_midi().unwrap();
    let mut keys: Vec<u8> = pc.notes.iter().map(|n| n.key).collect();
    keys.sort_unstable();
    assert_eq!(keys, [50, 53, 60, 62, 72]);
    assert!(pc.notes.iter().all(|n| n.channel == 1));
    assert_eq!(
        pc.lane(MidiController::Program, 1).unwrap().points[0].value,
        0
    );
    // Keys, chords, words.
    let ks: Vec<(f64, Key)> = p.keys.iter().map(|k| (k.at.quarters(), k.key)).collect();
    assert_eq!(
        ks,
        [
            (0.0, Key::new(5, Scale::Major)),
            (6.0, Key::new(2, Scale::Minor))
        ]
    );
    let chords: Vec<(f64, f64, Chord)> = p
        .chords
        .iter()
        .map(|c| (c.start.quarters(), c.end.quarters(), c.chord))
        .collect();
    assert_eq!(
        chords,
        [
            (3.0, 6.0, Chord::new(5, Quality::Major)),
            (6.0, 9.0, Chord::new(2, Quality::Minor))
        ]
    );
    let words: Vec<&str> = p.lyrics.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(words, ["Come dancing slow"]);
    assert_eq!((p.lyrics[0].start, p.lyrics[0].end), (q(2.0), q(9.0)));
    // One undo step takes it all back.
    s.dispatch(Action::Undo).unwrap();
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks_before);
    assert!(p.keys.is_empty() && p.chords.is_empty() && p.lyrics.is_empty());
    assert!((p.timeline.tempo.points()[0].bpm - 120.0).abs() < 1e-9);
}

#[test]
fn a_lead_sheet_comes_back_into_a_project() {
    use faderframe_leadsheet::build::{Bar, ChordAt, Input, Line, Note, Staff};
    let tune: Vec<Note> = [
        (0.0, 1.0, 67),
        (1.0, 2.0, 69),
        (2.0, 4.0, 71),
        (4.0, 4.5, 72),
        (4.5, 5.0, 71),
        (5.0, 8.0, 69),
    ]
    .iter()
    .map(|&(start, end, key)| Note { start, end, key })
    .collect();
    let sheet = faderframe_leadsheet::build(&Input {
        title: "Song".into(),
        key: Some(Key::new(7, Scale::Major)),
        bars: (0..2)
            .map(|i| Bar {
                start: f64::from(i) * 4.0,
                time: (4, 4),
            })
            .collect(),
        tempo: Some(110.0),
        staves: vec![Staff {
            name: "Lead".into(),
            notes: tune.clone(),
            lines: vec![
                Line {
                    start: 0.0,
                    end: 4.0,
                    text: "Here we go".into(),
                },
                Line {
                    start: 4.0,
                    end: 8.0,
                    text: "again".into(),
                },
            ],
            ..Staff::default()
        }],
        chords: vec![
            ChordAt {
                start: 0.0,
                chord: Chord::new(7, Quality::Major),
            },
            ChordAt {
                start: 4.0,
                chord: Chord::new(2, Quality::Dominant7),
            },
        ],
        ..Input::default()
    });
    let dir = tmp("leadsheet");
    let xml = faderframe_leadsheet::musicxml::write(&sheet, "2026-11-04");
    // Compressed, as notation programs share them.
    let path = dir.join("Song.mxl");
    let mut w = ZipWriter::create(&path).unwrap();
    w.add("mimetype", b"application/vnd.recordare.musicxml", false)
        .unwrap();
    w.add(
        "META-INF/container.xml",
        br#"<container><rootfiles><rootfile full-path="Song.musicxml"/></rootfiles></container>"#,
        true,
    )
    .unwrap();
    w.add("Song.musicxml", xml.as_bytes(), true).unwrap();
    w.finish().unwrap();

    // Into a project with music already: at bar 3, its tempo left.
    let mut s = empty();
    s.dispatch(Action::ImportMusicXml {
        path: path.clone(),
        at: q(8.0),
        tempo: false,
    })
    .unwrap();
    let p = s.project();
    assert!((p.timeline.tempo.points()[0].bpm - 120.0).abs() < 1e-9);
    let lead = p.tracks.iter().find(|t| t.name == "Lead").unwrap();
    let c = p.clips_of(lead.id)[0];
    assert_eq!(c.start, q(8.0));
    let m = c.as_midi().unwrap();
    let back: Vec<(f64, f64, u8)> = m
        .notes
        .iter()
        .map(|n| (n.start.quarters(), n.end().quarters(), n.key))
        .collect();
    let want: Vec<(f64, f64, u8)> = tune.iter().map(|n| (n.start, n.end, n.key)).collect();
    assert_eq!(back, want);
    assert_eq!(p.keys[0].at, q(8.0));
    assert_eq!(p.keys[0].key, Key::new(7, Scale::Major));
    let chords: Vec<(f64, Chord)> = p
        .chords
        .iter()
        .map(|c| (c.start.quarters(), c.chord))
        .collect();
    assert_eq!(
        chords,
        [
            (8.0, Chord::new(7, Quality::Major)),
            (12.0, Chord::new(2, Quality::Dominant7))
        ]
    );
    let words: Vec<(&str, f64)> = p
        .lyrics
        .iter()
        .map(|l| (l.text.as_str(), l.start.quarters()))
        .collect();
    assert_eq!(words, [("Here we go", 8.0), ("again", 12.0)]);
}

#[test]
fn a_file_without_music_is_refused() {
    let dir = tmp("empty");
    let path = dir.join("Empty.musicxml");
    std::fs::write(
        &path,
        r#"<score-partwise version="4.0"><part-list><score-part id="P1"><part-name>X</part-name></score-part></part-list>
           <part id="P1"><measure number="1"><attributes><divisions>1</divisions></attributes><note><rest/><duration>4</duration></note></measure></part></score-partwise>"#,
    )
    .unwrap();
    let mut s = empty();
    assert!(s.import_musicxml(&path, MusicalTime::ZERO, true).is_err());
    assert!(
        s.import_musicxml(&dir.join("nope.xml"), MusicalTime::ZERO, true)
            .is_err()
    );
}
