#![allow(clippy::unwrap_used)]
//! The project's key and chord track: detected from MIDI clips (one undo
//! step each), and kept through section moves.

use faderframe_engine::EngineConfig;
use faderframe_project::harmony::{Key, Scale};
use faderframe_project::{Project, TrackKind};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

#[test]
fn chords_and_the_key_come_from_the_notes() {
    let mut s = Session::new(Project::new("Song", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: q(0.0),
        length: q(16.0),
    })
    .unwrap();
    let clip = s.project().track(t).unwrap().clips[0];
    // Am | F | C | G, a bar each, in close position.
    for (bar, chord) in [[57, 60, 64], [53, 57, 60], [55, 60, 64], [55, 59, 62]]
        .iter()
        .enumerate()
    {
        for key in chord {
            s.dispatch(Action::AddNote {
                clip,
                start: q(4.0 * bar as f64),
                length: q(4.0),
                key: *key,
                velocity: 90,
            })
            .unwrap();
        }
    }
    s.dispatch(Action::DetectChords).unwrap();
    let names: Vec<String> = s
        .project()
        .chords
        .iter()
        .map(|c| c.chord.name(false))
        .collect();
    assert_eq!(names, ["Am", "F", "C/G", "G"]);
    assert_eq!(s.project().chords[1].start, q(4.0));
    s.dispatch(Action::DetectKey).unwrap();
    assert_eq!(s.project().key_at(q(0.0)), Some(Key::new(0, Scale::Major)));
    // Each is one undo step.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().keys.is_empty());
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().chords.is_empty());
}

/// The piano roll follows the key track: its scale changes where the key
/// does, snaps and scale chords use the key at the note, and the chord
/// track's chord can be stamped.
#[test]
fn the_piano_roll_follows_the_key_and_chord_tracks() {
    use faderframe_project::harmony::{Chord, Quality};
    use faderframe_project::midi_ops::{ChordKind, ScaleKind};
    use faderframe_project::{ChordEvent, ClipContent, Command, KeyChange};
    let mut s = Session::new(Project::new("Song", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: q(0.0),
        length: q(16.0),
    })
    .unwrap();
    let clip = s.project().track(t).unwrap().clips[0];
    // Without keys: the chosen scale (chromatic) everywhere.
    assert!(!s.piano_follows_key());
    assert!(s.piano_scale_at(q(0.0)).is_chromatic());
    // C major, then D major from bar 3.
    s.dispatch(Action::Edit(Command::SetKeys {
        keys: vec![
            KeyChange {
                at: q(0.0),
                key: Key::new(0, Scale::Major),
            },
            KeyChange {
                at: q(8.0),
                key: Key::new(2, Scale::Major),
            },
        ],
    }))
    .unwrap();
    assert!(s.piano_follows_key());
    let c = s.piano_scale_at(q(4.0));
    assert_eq!((c.root, c.kind), (0, ScaleKind::Major));
    assert_eq!(s.piano_scale_at(q(9.0)).root, 2);
    let spans = s.piano_scales(q(0.0), q(16.0));
    assert_eq!(spans.len(), 2);
    assert_eq!((spans[0].1, spans[1].0), (q(8.0), q(8.0)));
    // Scale snap and scale triads use the key at the note: F# (66) is in
    // D major, but moves to F in C major (ties go down); the triad on D is
    // D F# A in D major, D F A in C major.
    let mut pr = s.editor.piano;
    pr.scale_snap = true;
    pr.chord = ChordKind::Single;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    assert_eq!(s.piano_chord_keys(66, q(1.0)), vec![65]);
    assert_eq!(s.piano_chord_keys(66, q(9.0)), vec![66]);
    pr.chord = ChordKind::ScaleTriad;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    assert_eq!(s.piano_chord_keys(62, q(9.0)), vec![62, 66, 69]);
    assert_eq!(s.piano_chord_keys(62, q(1.0)), vec![62, 65, 69]);
    // The chord track's chord, voiced round the clicked key; a scale
    // triad where there is none.
    s.dispatch(Action::Edit(Command::SetChords {
        chords: vec![ChordEvent {
            start: q(0.0),
            end: q(4.0),
            chord: Chord::new(9, Quality::ALL[1]),
        }],
    }))
    .unwrap();
    let am = s.project().chords[0].chord;
    pr.chord = ChordKind::ChordTrack;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    let keys = s.piano_chord_keys(58, q(1.0));
    let expected: Vec<u8> = am.voicing(58).into_iter().map(|k| k as u8).collect();
    assert_eq!(keys, expected);
    assert!(keys.iter().all(|k| am.contains(i32::from(*k))));
    assert_eq!(s.piano_chord_keys(62, q(9.0)), vec![62, 66, 69]);
    // Drawn into the clip: one undo step, the chord track's notes.
    s.dispatch(Action::AddChord {
        clip,
        start: q(0.0),
        length: q(1.0),
        key: 58,
        velocity: 100,
    })
    .unwrap();
    let ClipContent::Midi(m) = &s.project().clip(clip).unwrap().content else {
        panic!("a MIDI clip")
    };
    let mut drawn: Vec<u8> = m.notes.iter().map(|n| n.key).collect();
    drawn.sort_unstable();
    assert_eq!(drawn, expected);
    // Not following: the chosen scale again.
    pr.follow_key = false;
    pr.chord = ChordKind::Single;
    s.dispatch(Action::SetPianoRoll(pr)).unwrap();
    assert!(!s.piano_follows_key());
    assert_eq!(s.piano_chord_keys(66, q(1.0)), vec![66]);
}
