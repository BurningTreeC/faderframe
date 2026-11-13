//! Audio to MIDI: a sung phrase, chords and a drum pattern become MIDI
//! clips on new instrument tracks, in one undo step each; so does one take
//! of a take folder.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::{AudioRef, ClipContent, Command, Project, Take, TakeFolder, TrackKind};
use faderframe_session::to_midi::ToMidi;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

/// Harmonic tones: each `(keys, seconds)` in turn.
fn tones(parts: &[(&[u8], f64)]) -> Vec<f32> {
    let mut out = Vec::new();
    for (keys, seconds) in parts {
        let n = (seconds * f64::from(SR)) as usize;
        for i in 0..n {
            let t = i as f64 / f64::from(SR);
            let env = (t / 0.01).min(1.0) * ((seconds - t) / 0.02).clamp(0.0, 1.0);
            let v: f64 = keys
                .iter()
                .map(|k| {
                    let f = 440.0 * 2f64.powf((f64::from(*k) - 69.0) / 12.0);
                    (1..=6)
                        .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / (h * h) as f64)
                        .sum::<f64>()
                })
                .sum();
            out.push((v * 0.25 * env) as f32);
        }
        out.extend(std::iter::repeat_n(0.0, (0.06 * f64::from(SR)) as usize));
    }
    out
}

/// Kick, hat, snare, hat … at 120 BPM eighths, two bars.
fn drums() -> Vec<f32> {
    let step = 0.25;
    let mut x = vec![0.0f32; (16.0 * step * f64::from(SR)) as usize];
    let mut seed = 5u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
    };
    let mut hp = 0.0f32;
    for k in 0..16 {
        let at = (k as f64 * step * f64::from(SR)) as usize;
        for i in 0..9_600 {
            let t = i as f64 / f64::from(SR);
            let v = match k % 4 {
                // A kick: a falling sine.
                0 => {
                    ((std::f64::consts::TAU * (55.0 + 90.0 * (-t / 0.03).exp()) * t).sin()
                        * (-t / 0.12).exp()) as f32
                }
                // A snare: a tone and mid noise.
                2 => {
                    ((std::f64::consts::TAU * 190.0 * t).sin() * 0.4 * (-t / 0.05).exp()) as f32
                        + noise() * 0.6 * (-(t / 0.08)).exp() as f32
                }
                // A hat: bright noise.
                _ => {
                    let n = noise();
                    let h = n - hp;
                    hp = n;
                    h * 0.5 * (-(t / 0.02)).exp() as f32
                }
            };
            if let Some(s) = x.get_mut(at + i) {
                *s += v * 0.8;
            }
        }
    }
    x
}

fn session(name: &str, samples: Vec<f32>) -> (Session, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-session-to-midi-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{name}.wav"));
    write_wav(&file, &[samples], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("To MIDI", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::from_quarters(4.0),
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    (s, clip)
}

/// Notes found: start and end (seconds from the MIDI clip's start), key,
/// velocity.
type Notes = Vec<(f64, f64, u8, u8)>;

/// Convert and wait; the notes (seconds from the audio clip's start, key,
/// velocity) and the new track's instrument.
fn convert(s: &mut Session, clip: ClipId, how: ToMidi) -> (Notes, String) {
    let (notes, instrument, start, _) = convert_audio(s, clip.into(), how);
    assert_eq!(start, MusicalTime::from_quarters(4.0), "where the audio is");
    (notes, instrument)
}

/// Convert a clip or take and wait; the notes (seconds from the MIDI
/// clip's start, key, velocity), the new track's instrument, where the
/// MIDI clip starts and what it is called.
fn convert_audio(
    s: &mut Session,
    audio: AudioRef,
    how: ToMidi,
) -> (Notes, String, MusicalTime, String) {
    let tracks = s.project().tracks.len();
    s.dispatch(Action::ConvertToMidi { audio, how }).unwrap();
    let start = Instant::now();
    while s.converting_to_midi() {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "conversion hangs"
        );
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks + 1, "a new track");
    let t = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap();
    let c = p.clip(t.clips[0]).unwrap();
    let m = c.as_midi().unwrap();
    let rate = f64::from(SR);
    let base = p.timeline.to_samples(c.start, rate);
    let secs = |q: MusicalTime| (p.timeline.to_samples(c.start + q, rate) - base) as f64 / rate;
    let notes = m
        .notes
        .iter()
        .map(|n| (secs(n.start), secs(n.start + n.length), n.key, n.velocity))
        .collect();
    (
        notes,
        t.inserts[0].plugin.id.clone(),
        c.start,
        c.name.clone(),
    )
}

#[test]
fn a_sung_phrase_becomes_a_melody() {
    let (mut s, clip) = session(
        "Line",
        tones(&[(&[60], 0.4), (&[62], 0.4), (&[64], 0.6), (&[60], 0.4)]),
    );
    let (notes, instrument) = convert(&mut s, clip, ToMidi::Melody);
    assert_eq!(instrument, faderframe_core::builtin::SYNTH);
    let keys: Vec<u8> = notes.iter().map(|n| n.2).collect();
    assert_eq!(keys, [60, 62, 64, 60], "{notes:?}");
    // Each where it was sung (the notes 0.46 s apart).
    for (n, at) in notes.iter().zip([0.0, 0.46, 0.92, 1.58]) {
        assert!((n.0 - at).abs() < 0.04, "{n:?} vs {at}");
        assert!(n.3 >= 80, "loud: {n:?}");
    }
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Instrument)
            .count(),
        0
    );
}

#[test]
fn chords_become_their_notes() {
    let (mut s, clip) = session(
        "Chords",
        tones(&[(&[60, 64, 67], 1.0), (&[57, 60, 64], 1.0)]),
    );
    let (notes, _) = convert(&mut s, clip, ToMidi::Harmony);
    let strong = |t: f64| {
        let mut k: Vec<u8> = notes
            .iter()
            .filter(|n| n.0 <= t && t < n.1 && n.3 > 70)
            .map(|n| n.2)
            .collect();
        k.sort_unstable();
        k
    };
    assert_eq!(strong(0.5), [60, 64, 67], "{notes:?}");
    assert_eq!(strong(1.6), [57, 60, 64], "{notes:?}");
}

#[test]
fn a_drum_pattern_becomes_hits_on_the_pads() {
    let (mut s, clip) = session("Beat", drums());
    let (notes, instrument) = convert(&mut s, clip, ToMidi::Drums);
    assert_eq!(instrument, faderframe_core::builtin::DRUMS);
    // Kick, hat, snare, hat a quarter second apart.
    let expect = [36, 42, 38, 42];
    let mut matched = 0;
    for (k, want) in expect.iter().cycle().take(16).enumerate() {
        let at = k as f64 * 0.25;
        let hit = notes.iter().find(|n| (n.0 - at).abs() < 0.03);
        if hit.is_some_and(|n| n.2 == *want) {
            matched += 1;
        }
    }
    assert!(matched >= 14, "{matched} of 16 right: {notes:?}");
}

/// A take is a clip of its own: one take of a folder turns into MIDI on
/// its own — its notes, from where it starts in the folder.
#[test]
fn one_take_of_a_folder_becomes_its_notes() {
    let (mut s, clip) = session(
        "Takes",
        tones(&[(&[60], 0.4), (&[62], 0.4), (&[64], 0.6), (&[60], 0.4)]),
    );
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    // Take 2 is the phrase from its second note on, where it was sung.
    let later = (0.46 * f64::from(SR)) as i64;
    let mut f = TakeFolder::new(a.length);
    f.add_take(Take::of_clip("Take 1", 0, a.clone()));
    f.add_take(Take::of_clip(
        "Take 2",
        later,
        a.cut(later, a.length).unwrap(),
    ));
    f.use_take(0);
    s.edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    let two = s.project().take_ref(clip, 1).unwrap();
    let (notes, instrument, start, name) = convert_audio(&mut s, two.into(), ToMidi::Melody);
    assert_eq!(instrument, faderframe_core::builtin::SYNTH);
    assert_eq!(name, format!("{} · Take 2 Melody", c.name));
    let p = s.project();
    let rate = f64::from(SR);
    assert_eq!(
        p.timeline.to_samples(start, rate),
        p.timeline.to_samples(c.start, rate) + later,
        "where the take starts"
    );
    let keys: Vec<u8> = notes.iter().map(|n| n.2).collect();
    assert_eq!(keys, [62, 64, 60], "{notes:?}");
    for (n, at) in notes.iter().zip([0.0, 0.46, 1.12]) {
        assert!((n.0 - at).abs() < 0.04, "{n:?} vs {at}");
    }
}
