//! Polyphonic pitch editing: a recorded chord's notes found (every one),
//! one of them moved, the clip playing the render; undo and redo bring back
//! notes and audio together.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Project};
use faderframe_session::pitch::PitchOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::f64::consts::PI;

const SR: u32 = 48_000;

fn hz(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// A harmonic tone (12 partials, 1/h) over `a..b` seconds.
fn tone(x: &mut [f32], key: f64, a: f64, b: f64, gain: f64) {
    let rate = f64::from(SR);
    let f = hz(key);
    for i in (a * rate) as usize..((b * rate) as usize).min(x.len()) {
        let t = i as f64 / rate - a;
        let env = (t / 0.01).min(1.0) * ((b - a - t) / 0.02).clamp(0.0, 1.0);
        let mut v = 0.0;
        for h in 1..=12 {
            v += (2.0 * PI * f * f64::from(h) * t).sin() / f64::from(h);
        }
        x[i] += (v * env * gain) as f32;
    }
}

/// The level (dB) of `f` around second `t` (Hann, 8192).
fn level(x: &[f32], t: f64, f: f64) -> f64 {
    let n = 8192usize;
    let c = (t * f64::from(SR)) as usize;
    let (mut re, mut im) = (0.0, 0.0);
    for i in 0..n {
        let s = f64::from(x[c - n / 2 + i]);
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
        let a = 2.0 * PI * f * i as f64 / f64::from(SR);
        re += s * w * a.cos();
        im -= s * w * a.sin();
    }
    20.0 * ((re * re + im * im).sqrt() / n as f64).max(1e-12).log10()
}

#[test]
fn a_note_of_a_recorded_chord_moves_and_undo_brings_it_back() {
    let dir = std::env::temp_dir().join(format!("ff-polypitch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("chord.wav");
    let mut x = vec![0.0f32; 3 * SR as usize];
    for key in [60.0, 64.0, 67.0] {
        tone(&mut x, key, 0.3, 2.5, 0.12);
    }
    write_wav(&file, &[x.clone()], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Chord", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    assert!(s.editor.pitch_polyphonic, "polyphonic by default");
    s.dispatch(Action::OpenPitchEditor(clip)).unwrap();
    s.wait_for_polyphonic();
    let audio = |s: &Session| s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    let a = audio(&s);
    let e = a.pitch.clone().expect("notes found");
    let poly = e.polyphonic.expect("polyphonic");
    let original = a.source;
    assert_eq!(poly.original, original);
    // The three notes, each where it was played (more may be found:
    // they are left alone).
    for key in [60.0f32, 64.0, 67.0] {
        assert!(
            e.notes.iter().any(|n| (n.pitch - key).abs() < 0.1),
            "{key} found: {:?}",
            e.notes.iter().map(|n| n.pitch).collect::<Vec<_>>()
        );
    }
    let e4 = e
        .notes
        .iter()
        .position(|n| (n.pitch - 64.0).abs() < 0.1)
        .unwrap();
    // E down a semitone: the clip plays a render with E♭.
    s.dispatch(Action::EditPitch {
        clip,
        op: PitchOp::Move {
            notes: vec![e4],
            by: -1.0,
        },
    })
    .unwrap();
    s.wait_for_polyphonic();
    let moved = audio(&s);
    assert_ne!(moved.source, original, "a render plays");
    let ed = moved.pitch.clone().unwrap();
    assert_eq!(ed.polyphonic.unwrap().rendered, ed.sound_key());
    let out = s.source_frames(moved.source, 0, 3 * SR as usize).unwrap();
    let t = 1.4;
    let (e, eb) = (hz(64.0), hz(63.0));
    let gone = level(&x, t, e) - level(&out[0], t, e);
    let came = level(&out[0], t, eb) - level(&x, t, eb);
    assert!(
        gone > 15.0 && came > 15.0,
        "E down {gone:.1} dB, E♭ up {came:.1} dB"
    );
    for key in [60.0, 67.0] {
        let d = level(&out[0], t, hz(key)) - level(&x, t, hz(key));
        assert!(d.abs() < 1.5, "{key}: {d:.2} dB");
    }
    // Undo: the notes as played and the recording itself, together.
    s.dispatch(Action::Undo).unwrap();
    let back = audio(&s);
    assert_eq!(back.source, original);
    assert!(!back.pitch.as_ref().unwrap().edited());
    s.wait_for_polyphonic();
    assert_eq!(audio(&s).source, original, "nothing to render");
    // Redo: the moved note and its render.
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(audio(&s).source, moved.source);
    // Muting a note re-renders; removing the edit plays the recording.
    s.dispatch(Action::EditPitch {
        clip,
        op: PitchOp::Mute {
            notes: vec![e4],
            on: true,
        },
    })
    .unwrap();
    s.wait_for_polyphonic();
    let muted = audio(&s);
    assert_ne!(muted.source, moved.source);
    let out = s.source_frames(muted.source, 0, 3 * SR as usize).unwrap();
    let gone = level(&x, t, hz(64.0)) - level(&out[0], t, hz(64.0));
    assert!(gone > 15.0, "E muted: down {gone:.1} dB");
    assert!(
        level(&out[0], t, hz(63.0)) - level(&x, t, hz(63.0)) < 6.0,
        "not moved any more"
    );
    s.dispatch(Action::EditPitch {
        clip,
        op: PitchOp::Remove,
    })
    .unwrap();
    let plain = audio(&s);
    assert_eq!(plain.source, original);
    assert!(plain.pitch.is_none());
    let _ = matches!(
        s.project().clip(clip).unwrap().content,
        ClipContent::Audio(_)
    );
}
