//! Pitch editing through the session: a sung phrase's notes are found,
//! moved (a drag is one lossless step), corrected to the key, split,
//! joined, reset and removed.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::harmony::{Key, KeyChange, Scale};
use faderframe_project::{Command, Project};
use faderframe_session::pitch::PitchOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

/// A voice-like phrase: each note `(MIDI, seconds)` with a short gap.
fn phrase(notes: &[(f64, f64)]) -> Vec<f32> {
    let mut out = Vec::new();
    for &(note, seconds) in notes {
        let f = 440.0 * 2f64.powf((note - 69.0) / 12.0);
        let n = (seconds * f64::from(SR)) as usize;
        for i in 0..n {
            let t = i as f64 / f64::from(SR);
            let env = (t / 0.01).min(1.0) * ((seconds - t) / 0.02).clamp(0.0, 1.0);
            let v: f64 = (1..=8)
                .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / (h * h) as f64)
                .sum();
            out.push((v * 0.3 * env) as f32);
        }
        out.extend(std::iter::repeat_n(0.0, (0.08 * f64::from(SR)) as usize));
    }
    out
}

/// A session with the phrase imported; returns it and its clip.
fn session(notes: &[(f64, f64)]) -> (Session, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-session-pitch-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("Voice{}.wav", notes.len()));
    write_wav(&file, &[phrase(notes)], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Pitch", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    (s, clip)
}

fn detect(s: &mut Session, clip: ClipId) {
    s.dispatch(Action::DetectPitch { clips: vec![clip] })
        .unwrap();
    let start = Instant::now();
    while s.detecting_pitch() {
        assert!(start.elapsed() < Duration::from_secs(30), "detection hangs");
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

/// (pitch, shift, drift, formant) of each note.
fn notes(s: &Session, clip: ClipId) -> Vec<(f32, f32, f32, f32)> {
    s.project()
        .clip(clip)
        .unwrap()
        .as_audio()
        .unwrap()
        .pitch
        .as_ref()
        .map(|e| {
            e.notes
                .iter()
                .map(|n| (n.pitch, n.shift, n.drift, n.formant))
                .collect()
        })
        .unwrap_or_default()
}

fn edit(s: &mut Session, clip: ClipId, op: PitchOp) {
    s.dispatch(Action::EditPitch { clip, op }).unwrap();
}

#[test]
fn a_phrase_is_found_moved_and_corrected() {
    // C4 a little sharp, D4, E4 a little flat.
    let (mut s, clip) = session(&[(60.3, 0.4), (62.0, 0.4), (63.8, 0.4)]);
    // Not analysed: nothing to edit.
    assert!(
        s.dispatch(Action::EditPitch {
            clip,
            op: PitchOp::Reset { notes: Vec::new() },
        })
        .is_err()
    );
    detect(&mut s, clip);
    let n = notes(&s, clip);
    let sung: Vec<f32> = n.iter().map(|x| x.0).collect();
    assert_eq!(sung.len(), 3, "{sung:?}");
    for (got, want) in sung.iter().zip([60.3, 62.0, 63.8]) {
        assert!((got - want).abs() < 0.05, "{sung:?}");
    }
    let steps = s.history_steps().0.len();
    // A drag: the total move each time, one step at the end.
    s.dispatch(Action::BeginGesture("Move Notes".into()))
        .unwrap();
    for by in [0.5, 1.5, 2.0] {
        edit(&mut s, clip, PitchOp::Move { notes: vec![1], by });
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(notes(&s, clip)[1].1, 2.0, "the last total, not the sum");
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // Corrected in A minor: C and E, the D moved up a whole tone is E.
    s.dispatch(Action::Edit(Command::SetKeys {
        keys: vec![KeyChange {
            at: MusicalTime::ZERO,
            key: Key::new(9, Scale::Minor),
        }],
    }))
    .unwrap();
    edit(
        &mut s,
        clip,
        PitchOp::Correct {
            notes: Vec::new(),
            amount: 1.0,
            drift: 0.6,
        },
    );
    let heard: Vec<f32> = notes(&s, clip).iter().map(|x| x.0 + x.1).collect();
    for (got, want) in heard.iter().zip([60.0, 64.0, 64.0]) {
        assert!((got - want).abs() < 1e-4, "{heard:?}");
    }
    assert!(notes(&s, clip).iter().all(|x| x.2 == 0.6));
    // Half way only.
    edit(&mut s, clip, PitchOp::Reset { notes: vec![0] });
    edit(
        &mut s,
        clip,
        PitchOp::Correct {
            notes: vec![0],
            amount: 0.5,
            drift: 0.0,
        },
    );
    assert!((notes(&s, clip)[0].1 + 0.15).abs() < 1e-3);
    // Formants on their own.
    edit(
        &mut s,
        clip,
        PitchOp::Set {
            notes: vec![2],
            shift: None,
            drift: None,
            formant: Some(-3.0),
        },
    );
    assert_eq!(notes(&s, clip)[2].3, -3.0);
    // Undo steps back one edit at a time.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(notes(&s, clip)[2].3, 0.0);
}

#[test]
fn notes_split_join_and_the_edit_goes() {
    let (mut s, clip) = session(&[(57.0, 0.8)]);
    detect(&mut s, clip);
    assert_eq!(notes(&s, clip).len(), 1);
    let (start, end) = {
        let a = s.project().clip(clip).unwrap().as_audio().unwrap();
        let n = &a.pitch.as_ref().unwrap().notes[0];
        (n.start, n.end)
    };
    edit(
        &mut s,
        clip,
        PitchOp::Split {
            note: 0,
            at: (start + end) / 2,
        },
    );
    assert_eq!(notes(&s, clip).len(), 2);
    edit(&mut s, clip, PitchOp::Join { notes: vec![0, 1] });
    let n = notes(&s, clip);
    assert_eq!(n.len(), 1);
    assert!((n[0].0 - 57.0).abs() < 0.05);
    edit(&mut s, clip, PitchOp::KeepFormants(false));
    let keep = |s: &Session| {
        s.project()
            .clip(clip)
            .unwrap()
            .as_audio()
            .unwrap()
            .pitch
            .as_ref()
            .map(|e| e.keep_formants)
    };
    assert_eq!(keep(&s), Some(false));
    edit(&mut s, clip, PitchOp::Remove);
    assert_eq!(keep(&s), None);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(keep(&s), Some(false));
}
