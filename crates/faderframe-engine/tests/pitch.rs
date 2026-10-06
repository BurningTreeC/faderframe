//! Pitch edits play through stretcher voices: a moved note sounds at its
//! new pitch while its neighbours stay, straightening takes the wandering
//! out of a note, and an unedited analysis changes nothing.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{AudioSourceId, ChannelLayout, ClipId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::render_project;
use faderframe_project::pitch::{PitchEdit, PitchNote};
use faderframe_project::{ClipContent, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
const HOP: u32 = 240;

/// A voice-like tone (harmonics falling off) following `pitch` (MIDI note
/// at a time in seconds).
fn tone(seconds: f64, pitch: impl Fn(f64) -> f64) -> Vec<f32> {
    let mut phase = 0.0f64;
    (0..(SR as f64 * seconds) as usize)
        .map(|i| {
            let t = i as f64 / SR as f64;
            phase = (phase + 440.0 * 2f64.powf((pitch(t) - 69.0) / 12.0) / SR as f64).fract();
            let v: f64 = (1..=8)
                .map(|h| (std::f64::consts::TAU * phase * h as f64).sin() / (h * h) as f64)
                .sum();
            (v * 0.4) as f32
        })
        .collect()
}

/// The pitch heard in `x` (MIDI), by McLeod's method.
fn pitch_of(x: &[f32]) -> f64 {
    let p = faderframe_analysis::pitch::Detector::new()
        .detect(x, f64::from(SR))
        .unwrap();
    assert!(p.clarity > 0.8, "unclear: {}", p.clarity);
    faderframe_analysis::pitch::note_at(p.freq, 440.0)
}

fn project(samples: Vec<f32>) -> (TestProject, ClipId) {
    let length = samples.len() as i64;
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "voice", ChannelLayout::Mono);
    let src: AudioSourceId = tp.source(AudioData::from_channels(SR, vec![samples]));
    tp.clip(t, src, MusicalTime::ZERO, length);
    let clip = *tp.project.track(t).unwrap().clips.last().unwrap();
    (tp, clip)
}

fn set_pitch(tp: &mut TestProject, clip: ClipId, edit: PitchEdit) {
    if let Some(ClipContent::Audio(a)) = tp.project.clips.get_mut(&clip).map(|c| &mut c.content) {
        a.pitch = Some(edit);
    }
}

fn render(tp: &TestProject, frames: usize) -> Vec<f32> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        0,
        frames,
    )
    .unwrap()
    .remove(0)
}

/// A steady note (curve at its pitch).
fn steady(start: i64, end: i64, pitch: f32) -> PitchNote {
    PitchNote {
        start,
        end,
        pitch,
        shift: 0.0,
        drift: 0.0,
        formant: 0.0,
        curve: vec![0; ((end - start) / i64::from(HOP)) as usize],
    }
}

#[test]
fn a_moved_note_sounds_at_its_new_pitch_and_its_neighbour_stays() {
    // A3 for a second, then E4.
    let (mut tp, clip) = project(tone(2.0, |t| if t < 1.0 { 57.0 } else { 64.0 }));
    let mut edit = PitchEdit {
        hop: HOP,
        notes: vec![steady(0, 48_000, 57.0), steady(48_000, 96_000, 64.0)],
        keep_formants: true,
    };
    // Analysed, nothing moved: played as recorded (no stretcher).
    set_pitch(&mut tp, clip, edit.clone());
    let plain = render(&tp, 96_000);
    assert!((pitch_of(&plain[22_000..26_096]) - 57.0).abs() < 0.05);
    // A3 up a fifth to E4; the E4 stays.
    edit.notes[0].shift = 7.0;
    set_pitch(&mut tp, clip, edit.clone());
    let out = render(&tp, 96_000);
    let first = pitch_of(&out[22_000..26_096]);
    let second = pitch_of(&out[70_000..74_096]);
    assert!((first - 64.0).abs() < 0.1, "moved: {first}");
    assert!((second - 64.0).abs() < 0.1, "kept: {second}");
    // Down instead, by 3.5 semitones (anywhere, not only semitones).
    edit.notes[0].shift = -3.5;
    set_pitch(&mut tp, clip, edit);
    let out = render(&tp, 96_000);
    let first = pitch_of(&out[22_000..26_096]);
    assert!((first - 53.5).abs() < 0.1, "{first}");
}

#[test]
fn straightening_takes_out_the_wandering() {
    // A3 with ±50 cents of vibrato at 5 Hz for two seconds.
    let vibrato = |t: f64| 0.5 * (std::f64::consts::TAU * 5.0 * t).sin();
    let (mut tp, clip) = project(tone(2.0, |t| 57.0 + vibrato(t)));
    let curve = (0..96_000 / HOP as usize)
        .map(|k| (vibrato(k as f64 * f64::from(HOP) / f64::from(SR)) * 100.0).round() as i16)
        .collect();
    let mut n = steady(0, 96_000, 57.0);
    n.curve = curve;
    n.drift = 1.0;
    set_pitch(
        &mut tp,
        clip,
        PitchEdit {
            hop: HOP,
            notes: vec![n],
            keep_formants: false,
        },
    );
    let out = render(&tp, 96_000);
    // Over 40 ms windows (a fifth of a vibrato cycle): the pitch hardly
    // moves any more, and sits at the note.
    let spread = |x: &[f32]| {
        let notes: Vec<f64> = x.chunks(1_920).map(pitch_of).collect();
        let lo = notes.iter().copied().fold(f64::MAX, f64::min);
        let hi = notes.iter().copied().fold(f64::MIN, f64::max);
        (lo, hi)
    };
    let (lo, hi) = spread(&tone(2.0, |t| 57.0 + vibrato(t))[24_000..72_000]);
    assert!(hi - lo > 0.6, "the source wanders: {lo}..{hi}");
    let (lo, hi) = spread(&out[24_000..72_000]);
    assert!(hi - lo < 0.15, "straightened: {lo}..{hi}");
    assert!(((lo + hi) / 2.0 - 57.0).abs() < 0.08, "{lo}..{hi}");
}

/// Brightness: the spectral centroid of `x` (Hz) over 4096 frames.
fn centroid(x: &[f32]) -> f64 {
    let n = 4096;
    let x = &x[..n];
    let (mut num, mut den) = (0.0, 0.0);
    for k in 1..600 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            let a = std::f64::consts::TAU * (k * i) as f64 / n as f64;
            re += f64::from(*v) * w * a.cos();
            im -= f64::from(*v) * w * a.sin();
        }
        let m = (re * re + im * im).sqrt();
        num += m * k as f64 * f64::from(SR) / n as f64;
        den += m;
    }
    num / den
}

#[test]
fn formants_stay_unless_moved() {
    // A buzz (A2, harmonics up to 6 kHz falling 6 dB an octave) through
    // a resonance near 1.2 kHz: a vowel-like colour.
    let f0 = 110.0;
    let buzz: Vec<f32> = (0..48_000)
        .map(|i| {
            let t = i as f64 / f64::from(SR);
            (1..=54)
                .map(|h| (std::f64::consts::TAU * f0 * h as f64 * t).sin() / h as f64)
                .sum::<f64>() as f32
                * 0.3
        })
        .collect();
    let (mut lp, mut bp) = (0.0f64, 0.0f64);
    let (f, q) = (
        2.0 * (std::f64::consts::PI * 1_200.0 / f64::from(SR)).sin(),
        0.15,
    );
    let voice: Vec<f32> = buzz
        .iter()
        .map(|x| {
            let hp = f64::from(*x) - lp - q * bp;
            bp += f * hp;
            lp += f * bp;
            (bp * 0.2) as f32
        })
        .collect();
    let original = centroid(&voice[20_000..]);
    let played = |keep: bool, formant: f32| {
        let (mut tp, clip) = project(voice.clone());
        let mut n = steady(0, 48_000, 45.0);
        n.shift = 7.0;
        n.formant = formant;
        set_pitch(
            &mut tp,
            clip,
            PitchEdit {
                hop: HOP,
                notes: vec![n],
                keep_formants: keep,
            },
        );
        let out = render(&tp, 48_000);
        (pitch_of(&out[20_000..24_096]), centroid(&out[20_000..]))
    };
    // Up a fifth either way.
    let (p_kept, kept) = played(true, 0.0);
    let (p_moved, moved) = played(false, 0.0);
    assert!((p_kept - 52.0).abs() < 0.05 && (p_moved - 52.0).abs() < 0.05);
    // Kept, the colour stays; moved with the pitch, it rises by a fifth.
    assert!((kept / original - 1.0).abs() < 0.1, "{kept} vs {original}");
    let fifth = 2f64.powf(7.0 / 12.0);
    assert!(
        (moved / original / fifth - 1.0).abs() < 0.12,
        "{moved} vs {original}"
    );
    // Moved on their own (down a fourth), the pitch stays put.
    let (p, lower) = played(true, -5.0);
    assert!((p - 52.0).abs() < 0.05);
    assert!(lower < original * 0.85, "{lower} vs {original}");
}

#[test]
fn playing_from_mid_note_starts_at_once_and_warping_comes_along() {
    use faderframe_project::{Warp, WarpAlgorithm};
    let (mut tp, clip) = project(tone(2.0, |_| 57.0));
    let mut n = steady(0, 96_000, 57.0);
    n.shift = 4.0;
    set_pitch(
        &mut tp,
        clip,
        PitchEdit {
            hop: HOP,
            notes: vec![n],
            keep_formants: true,
        },
    );
    // From the middle of the note: moved from the first frames on.
    let out = render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        50_000,
        8_192,
    )
    .unwrap()
    .remove(0);
    assert!((pitch_of(&out[..4096]) - 61.0).abs() < 0.05);
    let level = out[..512].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(level > 0.1, "no gap at the start: {level}");
    // Stretched to twice its length: the moved pitch, twice as long.
    if let Some(ClipContent::Audio(a)) = tp.project.clips.get_mut(&clip).map(|c| &mut c.content) {
        a.length = 192_000;
        a.warp = Some(Warp {
            source_length: 96_000,
            markers: Vec::new(),
            algorithm: WarpAlgorithm::Polyphonic,
        });
    }
    let out = render(&tp, 192_000);
    for at in [20_000, 90_000, 170_000] {
        assert!(
            (pitch_of(&out[at..at + 4096]) - 61.0).abs() < 0.05,
            "at {at}"
        );
    }
}
