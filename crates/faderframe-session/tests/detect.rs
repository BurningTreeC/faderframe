//! Tempo and key from clips: a loop's tempo becomes the project's (or the
//! loop is warped to the project's), its key the project's key.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::harmony::{Key, Scale};
use faderframe_project::{Command, Project};
use faderframe_session::detect::FromClip;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

/// Eight bars at 100 BPM: a kick on every beat (louder on one and three)
/// under Em Am B Em, two bars each.
fn groove() -> Vec<f32> {
    let beat = 60.0 / 100.0;
    let seconds = 32.0 * beat;
    let n = (seconds * f64::from(SR)) as usize;
    let chords: [&[u8]; 4] = [&[52, 55, 59], &[57, 60, 64], &[59, 63, 66], &[52, 55, 59]];
    let mut x = vec![0.0f32; n];
    for (i, s) in x.iter_mut().enumerate() {
        let t = i as f64 / f64::from(SR);
        let chord = chords[((t / (8.0 * beat)) as usize).min(3)];
        let v: f64 = chord
            .iter()
            .map(|k| {
                let f = 440.0 * 2f64.powf((f64::from(*k) - 69.0) / 12.0);
                (1..=5)
                    .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64)
                    .sum::<f64>()
            })
            .sum();
        // The kick: a falling sine burst at each beat.
        let into = t % beat;
        let k = (t / beat) as usize;
        let level = if k.is_multiple_of(2) { 1.0 } else { 0.6 };
        let kick = level
            * (-into / 0.06).exp()
            * (std::f64::consts::TAU * (50.0 + 120.0 * (-into / 0.02).exp()) * into).sin();
        *s = (v * 0.04 + kick * 0.6) as f32;
    }
    x
}

fn session() -> (Session, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-session-detect-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Groove.wav");
    write_wav(&file, &[groove()], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Detect", SR), None, EngineConfig::default()).unwrap();
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

fn from_clip(s: &mut Session, clip: ClipId, what: FromClip) {
    s.dispatch(Action::FromClip { clip, what }).unwrap();
    let start = Instant::now();
    while s.analysing_clips() {
        assert!(start.elapsed() < Duration::from_secs(60), "analysis hangs");
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

#[test]
fn a_loop_gives_its_tempo_and_key() {
    let (mut s, clip) = session();
    let bpm = |s: &Session| s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    assert_eq!(bpm(&s), 120.0);
    from_clip(&mut s, clip, FromClip::SetTempo);
    assert_eq!(bpm(&s), 100.0, "the loop's tempo");
    // Warped instead: the loop plays at the project's 120.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(bpm(&s), 120.0);
    let span = s.project().clip(clip).unwrap().as_audio().unwrap().length;
    from_clip(&mut s, clip, FromClip::WarpToTempo);
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_eq!(a.warp.as_ref().unwrap().source_length, span);
    assert_eq!(a.length, (span as f64 * 100.0 / 120.0).round() as i64);
    // The key: E minor.
    from_clip(&mut s, clip, FromClip::SetKey);
    assert_eq!(
        s.project().key_at(MusicalTime::ZERO),
        Some(Key::new(4, Scale::Minor))
    );
    // One undo step each.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().keys.is_empty());
}

#[test]
fn a_midi_clip_gives_its_key_at_once() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Edit(Command::SetKeys { keys: Vec::new() }))
        .unwrap();
    let melody = *s
        .project()
        .clips
        .values()
        .find(|c| c.name == "Melody")
        .map(|c| &c.id)
        .unwrap();
    s.dispatch(Action::FromClip {
        clip: melody,
        what: FromClip::SetKey,
    })
    .unwrap();
    assert!(!s.analysing_clips());
    let key = s.project().key_at(MusicalTime::ZERO).unwrap();
    // The demo's melody is in A minor (or its relative C major).
    assert!(
        key == Key::new(9, Scale::Minor) || key == Key::new(0, Scale::Major),
        "{key:?}"
    );
    // MIDI has no tempo to find.
    assert!(
        s.dispatch(Action::FromClip {
            clip: melody,
            what: FromClip::SetTempo,
        })
        .is_err()
    );
}
