//! Polyphonic pitch editing: a recorded chord's notes found (every one),
//! one of them moved, removed or made longer, the clip playing the render;
//! undo and redo bring back notes and audio together. Switching between
//! Polyphonic and Melodic finds the notes again the other way.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::pitch::{Edge, PitchEdit};
use faderframe_project::{ClipContent, Project};
use faderframe_session::pitch::PitchOp;
use faderframe_session::{Action, EditFlag, Session};
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

/// A session with `notes` (key, from, to seconds) recorded into a three
/// second clip; returns it, its clip and the recording.
fn recorded(name: &str, notes: &[(f64, f64, f64)]) -> (Session, ClipId, Vec<f32>) {
    let dir = std::env::temp_dir().join(format!("ff-polypitch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.wav"));
    let mut x = vec![0.0f32; 3 * SR as usize];
    for &(key, a, b) in notes {
        tone(&mut x, key, a, b, 0.12);
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
    (s, clip, x)
}

fn edit_of(s: &Session, clip: ClipId) -> PitchEdit {
    s.project()
        .clip(clip)
        .unwrap()
        .as_audio()
        .unwrap()
        .pitch
        .clone()
        .unwrap()
}

/// The first note at `key`.
fn note_at(e: &PitchEdit, key: f32) -> usize {
    e.notes
        .iter()
        .position(|n| (n.pitch - key).abs() < 0.3)
        .unwrap_or_else(|| {
            panic!(
                "{key} in {:?}",
                e.notes.iter().map(|n| n.pitch).collect::<Vec<_>>()
            )
        })
}

#[test]
fn a_note_of_a_chord_is_removed_and_another_made_longer() {
    let (mut s, clip, x) = recorded(
        "short-chord",
        &[(60.0, 0.3, 1.5), (64.0, 0.3, 1.5), (67.0, 0.3, 1.5)],
    );
    s.dispatch(Action::OpenPitchEditor(clip.into())).unwrap();
    s.wait_for_polyphonic();
    let e = edit_of(&s, clip);
    let (g, count) = (note_at(&e, 67.0), e.notes.len());
    // The G removed: gone from the editor and from the sound.
    s.dispatch(Action::EditPitch {
        audio: clip.into(),
        op: PitchOp::RemoveNotes { notes: vec![g] },
    })
    .unwrap();
    let e = edit_of(&s, clip);
    assert_eq!((e.notes.len(), e.removed.len()), (count - 1, 1));
    s.wait_for_polyphonic();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    let out = s.source_frames(a.source, 0, 3 * SR as usize).unwrap();
    let gone = level(&x, 0.9, hz(67.0)) - level(&out[0], 0.9, hz(67.0));
    assert!(gone > 15.0, "G down {gone:.1} dB");
    for key in [60.0, 64.0] {
        let d = level(&out[0], 0.9, hz(key)) - level(&x, 0.9, hz(key));
        assert!(d.abs() < 1.5, "{key}: {d:.2} dB");
    }
    // The E held on to 2.1 s: it rings on alone after the chord.
    let e4 = note_at(&e, 64.0);
    let end = e.notes[e4].end;
    s.dispatch(Action::EditPitch {
        audio: clip.into(),
        op: PitchOp::Length {
            notes: vec![e4],
            edge: Edge::End,
            by: (2.1 * f64::from(SR)) as i64 - end,
        },
    })
    .unwrap();
    s.wait_for_polyphonic();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    let out = s.source_frames(a.source, 0, 3 * SR as usize).unwrap();
    let held = level(&out[0], 1.8, hz(64.0)) - level(&x, 0.9, hz(64.0));
    assert!(held.abs() < 4.0, "E at 1.8 s: {held:.1} dB from as played");
    assert!(level(&x, 0.9, hz(60.0)) - level(&out[0], 1.8, hz(60.0)) > 30.0);
    assert!(level(&x, 0.9, hz(64.0)) - level(&x, 1.8, hz(64.0)) > 30.0);
    // Undo: as long as played again, the G still gone.
    s.dispatch(Action::Undo).unwrap();
    let e = edit_of(&s, clip);
    assert!(e.notes.iter().all(|n| n.timing.is_none()));
    assert_eq!(e.removed.len(), 1);
}

#[test]
fn switching_between_polyphonic_and_melodic_finds_the_notes_again() {
    let (mut s, clip, _) = recorded("line", &[(64.0, 0.3, 1.4), (67.0, 1.4, 2.5)]);
    let melodic = |s: &mut Session| {
        let start = std::time::Instant::now();
        while s.detecting_pitch() {
            assert!(start.elapsed().as_secs() < 30, "detection hangs");
            std::thread::sleep(std::time::Duration::from_millis(5));
            s.tick(0.005);
        }
    };
    let flag = |s: &mut Session, on: bool| {
        s.dispatch(Action::SetEditFlag(EditFlag::PolyphonicPitch, on))
            .unwrap();
    };
    // Switched while the notes are being found: found the new way only.
    s.dispatch(Action::OpenPitchEditor(clip.into())).unwrap();
    assert!(s.detecting_polyphonic());
    flag(&mut s, false);
    assert!(!s.detecting_polyphonic(), "the polyphonic search dropped");
    melodic(&mut s);
    s.wait_for_polyphonic();
    let e = edit_of(&s, clip);
    assert!(e.polyphonic.is_none(), "melodic");
    note_at(&e, 64.0);
    // Back to polyphonic: found again, with the E moved down first.
    let e4 = note_at(&e, 64.0);
    s.dispatch(Action::EditPitch {
        audio: clip.into(),
        op: PitchOp::Move {
            notes: vec![e4],
            by: -1.0,
        },
    })
    .unwrap();
    let original = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    let steps = s.history_steps().0.len();
    flag(&mut s, true);
    s.wait_for_polyphonic();
    let e = edit_of(&s, clip);
    assert!(e.polyphonic.is_some(), "polyphonic");
    assert_eq!(
        e.notes[note_at(&e, 64.0)].shift,
        -1.0,
        "the move carried over"
    );
    assert_eq!(e.notes[note_at(&e, 67.0)].shift, 0.0);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let rendered = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    assert_ne!(rendered, original, "a render plays");
    // Melodic again: the recording plays (live), the move still there.
    flag(&mut s, false);
    melodic(&mut s);
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    let e = a.pitch.clone().unwrap();
    assert!(e.polyphonic.is_none());
    assert_eq!(a.source, original, "the recording again");
    assert_eq!(e.notes[note_at(&e, 64.0)].shift, -1.0);
    // Undo: the polyphonic edit and its render back.
    s.dispatch(Action::Undo).unwrap();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert!(a.pitch.unwrap().polyphonic.is_some());
    assert_eq!(a.source, rendered);
}

#[test]
fn a_note_of_a_recorded_chord_moves_and_undo_brings_it_back() {
    let (mut s, clip, x) = recorded(
        "chord",
        &[(60.0, 0.3, 2.5), (64.0, 0.3, 2.5), (67.0, 0.3, 2.5)],
    );
    assert!(s.editor.pitch_polyphonic, "polyphonic by default");
    s.dispatch(Action::OpenPitchEditor(clip.into())).unwrap();
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
        audio: clip.into(),
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
        audio: clip.into(),
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
        audio: clip.into(),
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

/// The clip of `clip` made a folder of two takes of it (take 1 comped in
/// the first half, take 2 in the second); the folder's id is the clip's.
fn two_takes(s: &mut Session, clip: ClipId) {
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    let mut f = faderframe_project::TakeFolder::new(a.length);
    f.add_take(faderframe_project::Take::of_clip("Take 1", 0, a.clone()));
    f.add_take(faderframe_project::Take::of_clip("Take 2", 0, a.clone()));
    f.use_take(0);
    f.set_comp(a.length / 2, a.length, Some(1));
    s.edit(faderframe_project::Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
}

/// A take is a clip of its own: the pitch editor opens on one take of a
/// folder, finds its notes and moves one; that take plays a render of it
/// where the comp plays it, the other takes stay as they were; the editor
/// keeps to the take while takes before it go; undo takes the move back.
#[test]
fn one_take_of_a_folder_has_its_pitch_edited_on_its_own() {
    let (mut s, clip, x) = recorded(
        "take-chord",
        &[(60.0, 0.3, 2.5), (64.0, 0.3, 2.5), (67.0, 0.3, 2.5)],
    );
    let original = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    two_takes(&mut s, clip);
    let two = s.project().take_ref(clip, 1).unwrap();
    let take =
        |s: &Session, r: faderframe_project::TakeRef| s.project().take(r).unwrap().audio.clone();
    let one = s.project().take_ref(clip, 0).unwrap();

    s.dispatch(Action::OpenPitchEditor(two.into())).unwrap();
    assert_eq!(s.pitch_target(), Some(two.into()));
    s.wait_for_polyphonic();
    let e = take(&s, two).pitch.expect("its notes found");
    assert!(e.polyphonic.is_some());
    assert!(take(&s, one).pitch.is_none(), "the other take untouched");
    let e4 = note_at(&e, 64.0);
    s.dispatch(Action::EditPitch {
        audio: two.into(),
        op: PitchOp::Move {
            notes: vec![e4],
            by: -1.0,
        },
    })
    .unwrap();
    s.wait_for_polyphonic();
    let moved = take(&s, two);
    assert_ne!(moved.source, original, "the take plays a render");
    let ed = moved.pitch.clone().unwrap();
    assert_eq!(ed.polyphonic.unwrap().rendered, ed.sound_key());
    assert_eq!(take(&s, one).source, original, "take 1 plays the recording");
    let out = s.source_frames(moved.source, 0, 3 * SR as usize).unwrap();
    let (gone, came) = (
        level(&x, 1.4, hz(64.0)) - level(&out[0], 1.4, hz(64.0)),
        level(&out[0], 1.4, hz(63.0)) - level(&x, 1.4, hz(63.0)),
    );
    assert!(
        gone > 15.0 && came > 15.0,
        "E down {gone:.1}, E♭ up {came:.1}"
    );
    // Where the comp plays take 2, the render plays.
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    let sources: Vec<_> = f.played().iter().map(|p| p.audio.source).collect();
    assert_eq!(sources, vec![original, moved.source]);
    assert!(s.project().used_sources().contains(&original));

    // Take 1 deleted: the editor still shows take 2, which still edits.
    let mut fewer = f.clone();
    fewer.remove_take(0);
    s.edit(faderframe_project::Command::SetClipContent {
        clip,
        start: s.project().clip(clip).unwrap().start,
        content: Box::new(ClipContent::Takes(fewer)),
    })
    .unwrap();
    assert_eq!(s.pitch_target(), Some(two.into()));
    assert_eq!(s.project().take_in(two).map(|(_, i)| i), Some(0));
    s.dispatch(Action::EditPitch {
        audio: two.into(),
        op: PitchOp::Mute {
            notes: vec![e4],
            on: true,
        },
    })
    .unwrap();
    assert!(take(&s, two).pitch.unwrap().notes[e4].muted);
    // Undo both: take 1 back, take 2 with the E moved and its render.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(take(&s, two).source, moved.source);
    assert_eq!(take(&s, one).source, original);
    // And the move: take 2 as played.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(take(&s, two).source, original);
    assert!(!take(&s, two).pitch.unwrap().edited());
}

/// Melodic detection of several takes of one folder is one step.
#[test]
fn takes_of_a_folder_are_found_melodically_in_one_step() {
    let (mut s, clip, _) = recorded("take-line", &[(64.0, 0.3, 1.4), (67.0, 1.4, 2.5)]);
    two_takes(&mut s, clip);
    s.dispatch(Action::SetEditFlag(EditFlag::PolyphonicPitch, false))
        .unwrap();
    let takes: Vec<_> = (0..2)
        .map(|k| s.project().take_ref(clip, k).unwrap())
        .collect();
    let steps = s.history_steps().0.len();
    s.dispatch(Action::DetectPitch {
        audio: takes.iter().map(|&t| t.into()).collect(),
    })
    .unwrap();
    let start = std::time::Instant::now();
    while s.detecting_pitch() {
        assert!(start.elapsed().as_secs() < 30, "detection hangs");
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.tick(0.005);
    }
    for t in &takes {
        let e = s.project().take(*t).unwrap().audio.pitch.clone().unwrap();
        assert!(e.polyphonic.is_none(), "melodic");
        note_at(&e, 64.0);
        note_at(&e, 67.0);
    }
    assert_eq!(
        s.history_steps().0.len(),
        steps + 1,
        "one Detect Pitch step"
    );
}
