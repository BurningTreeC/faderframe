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
