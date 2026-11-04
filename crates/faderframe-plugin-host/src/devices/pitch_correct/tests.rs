#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{BLOCK, Rig, SR, silence, white};
use faderframe_midi::theory::{Chord, Key, Quality};
use faderframe_midi::{MidiBuffer, TimedMidiEvent};
use std::f64::consts::{PI, TAU};

fn hz(note: f64) -> f64 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

fn rig(set: &[(u32, f64)]) -> Rig<PitchCorrectProcessor> {
    Rig::with(parameters(), TAP_VALUES, set, PitchCorrectProcessor::new)
}

fn latency_frames(r: &Rig<PitchCorrectProcessor>) -> usize {
    latency(&r.params, SR) as usize
}

/// A two-pole resonator (unity gain at DC).
struct Resonator {
    a: f64,
    b: f64,
    c: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn new(f: f64, bw: f64) -> Self {
        let t = 1.0 / SR;
        let c = -(-TAU * bw * t).exp();
        let b = 2.0 * (-PI * bw * t).exp() * (TAU * f * t).cos();
        Self {
            a: 1.0 - b - c,
            b,
            c,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// `seconds` of a voice singing "ah" at `note(t)` (a glottal pulse
/// through three formants), made ahead so the rig can read any frame.
fn voice(seconds: f64, note: impl Fn(f64) -> f64) -> Vec<f32> {
    let n = (seconds * SR) as usize;
    let mut res = [
        Resonator::new(730.0, 80.0),
        Resonator::new(1090.0, 90.0),
        Resonator::new(2440.0, 120.0),
    ];
    let mut phase = 0.0;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR;
            phase += hz(note(t)) / SR;
            let p = phase.fract();
            let pulse = if p < 0.6 {
                (PI * p / 0.6).sin().powi(2)
            } else {
                0.0
            } - 0.3;
            let y: f64 = res
                .iter_mut()
                .zip([1.0, 0.6, 0.3])
                .map(|(r, w)| w * r.run(pulse))
                .sum();
            (0.5 * y) as f32
        })
        .collect()
}

fn sine(seconds: f64, note: f64) -> Vec<f32> {
    let f = hz(note);
    (0..(seconds * SR) as usize)
        .map(|i| (0.4 * (TAU * f * i as f64 / SR).sin()) as f32)
        .collect()
}

/// Play `x` from where the rig is (its output, as long).
fn play(r: &mut Rig<PitchCorrectProcessor>, x: &[f32]) -> Vec<f32> {
    let seconds = x.len() as f64 / SR;
    let from = r.at;
    let (mut l, _) = r.run(
        seconds,
        |i| {
            let v = x.get(i - from).copied().unwrap_or(0.0);
            (v, v)
        },
        silence,
    );
    l.truncate(x.len());
    l
}

/// The pitch (MIDI note) heard over `x` every 2 ms: (seconds, note).
fn heard(x: &[f32]) -> Vec<(f64, f64)> {
    let mut det = Detector::new(SR, 60.0, 1600.0);
    x.iter()
        .filter_map(|v| det.push(*v))
        .filter(|f| f.period > 0.0)
        .map(|f| (f.at / SR, note_of(f.period, SR)))
        .collect()
}

/// Where a voice's formants lie: the RMS frequency (Hz) of the second
/// half of `x` above 600 Hz (the fundamental and its octave out, so the
/// pitch itself counts little).
fn rms_hz(x: &[f32]) -> f64 {
    let mut y: Vec<f64> = x[x.len() / 2..].iter().map(|v| f64::from(*v)).collect();
    // Two RBJ high-passes at 600 Hz.
    let w = TAU * 600.0 / SR;
    let (sn, cs) = w.sin_cos();
    let alpha = sn / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let a0 = 1.0 + alpha;
    let (b0, b1, a1, a2) = (
        (1.0 + cs) / 2.0 / a0,
        -(1.0 + cs) / a0,
        -2.0 * cs / a0,
        (1.0 - alpha) / a0,
    );
    for _ in 0..2 {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in &mut y {
            let x0 = *v;
            let out = b0 * x0 + b1 * x1 + b0 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, x0, y1, out);
            *v = out;
        }
    }
    let tail = &y[y.len() / 4..];
    let e: f64 = tail.iter().map(|v| v * v).sum();
    let d: f64 = tail.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
    (d / e).sqrt() * SR / TAU
}

/// The mean note heard between `a` and `b` seconds, and its spread.
fn mean_between(x: &[f32], a: f64, b: f64) -> (f64, f64) {
    let notes: Vec<f64> = heard(x)
        .into_iter()
        .filter(|(t, _)| *t >= a && *t < b)
        .map(|(_, n)| n)
        .collect();
    assert!(!notes.is_empty(), "nothing heard between {a} and {b}");
    let mean = notes.iter().sum::<f64>() / notes.len() as f64;
    let spread = notes.iter().fold(0.0f64, |m, n| m.max((n - mean).abs()));
    (mean, spread)
}

#[test]
fn ids_are_positions() {
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i, "{}", p.name);
    }
    assert_eq!(parameters().len(), id::note(12) as usize);
}

#[test]
fn a_detuned_tone_is_pulled_to_pitch_within_the_retune_time() {
    // A4 sung 40 cents sharp; the nearest semitone is A4.
    let mut r = rig(&[
        (id::TARGET, target::CHROMATIC as f64),
        (id::SPEED, 60.0),
        (id::VIBRATO, 0.0),
    ]);
    let l = latency_frames(&r) as f64 / SR;
    let out = play(&mut r, &sine(1.2, 69.4));
    // Still sharp at first (the correction glides), on pitch within the
    // retune time.
    let (early, _) = mean_between(&out, l + 0.03, l + 0.045);
    assert!(early > 69.12, "{early}");
    let (late, spread) = mean_between(&out, l + 0.12, 1.15);
    assert!((late - 69.0).abs() < 0.03, "{late}");
    assert!(spread < 0.05, "{spread}");
    assert!((r.tap.value(value::TARGET) - 69.0).abs() < 1e-3);
    assert!((r.tap.value(value::CORRECTION) + 0.4).abs() < 0.03);
}

#[test]
fn instant_retune_holds_the_note_and_a_voice_keeps_its_formants() {
    let mut r = rig(&[
        (id::TARGET, target::CHROMATIC as f64),
        (id::SPEED, 0.0),
        (id::VIBRATO, 0.0),
    ]);
    let l = latency_frames(&r) as f64 / SR;
    // A voice wandering ±15 cents round 30 cents above G3.
    let x = voice(1.5, |t| 55.3 + 0.15 * (TAU * 3.0 * t).sin());
    let out = play(&mut r, &x);
    let (mean, spread) = mean_between(&out, l + 0.08, 1.45);
    assert!((mean - 55.0).abs() < 0.05, "{mean}");
    assert!(spread < 0.12, "{spread}");
    // The vowel stays where it was.
    let (a, b) = (rms_hz(&x), rms_hz(&out));
    assert!((b / a - 1.0).abs() < 0.06, "{a} {b}");
}

#[test]
fn natural_vibrato_is_kept_or_flattened_as_set() {
    // 15 cents sharp, a 5.5 Hz vibrato of ±40 cents.
    let x = sine(2.0, 69.15);
    let wobble = |t: f64| 69.15 + 0.4 * (TAU * 5.5 * t).sin();
    let x: Vec<f32> = {
        let mut phase = 0.0;
        (0..x.len())
            .map(|i| {
                phase += hz(wobble(i as f64 / SR)) / SR;
                (0.4 * (TAU * phase).sin()) as f32
            })
            .collect()
    };
    let depth = |vibrato: f64| {
        let mut r = rig(&[
            (id::TARGET, target::CHROMATIC as f64),
            (id::SPEED, 10.0),
            (id::VIBRATO, vibrato),
        ]);
        let out = play(&mut r, &x);
        let notes: Vec<f64> = heard(&out)
            .into_iter()
            .filter(|(t, _)| *t > 0.8)
            .map(|(_, n)| n)
            .collect();
        let mean = notes.iter().sum::<f64>() / notes.len() as f64;
        let lo = notes.iter().copied().fold(f64::MAX, f64::min);
        let hi = notes.iter().copied().fold(f64::MIN, f64::max);
        (mean, (hi - lo) / 2.0)
    };
    let (mean, kept) = depth(1.0);
    assert!((mean - 69.0).abs() < 0.06, "centred on the note: {mean}");
    assert!(kept > 0.3, "the vibrato kept: {kept}");
    let (mean, flat) = depth(0.0);
    assert!((mean - 69.0).abs() < 0.06, "{mean}");
    assert!(flat < 0.45 * kept, "flattened: {flat} (kept {kept})");
}

#[test]
fn the_key_track_and_the_chord_track_choose_the_notes() {
    // E♭4 + 30 cents: in C major the nearest note is E4, on a C7 chord
    // (C E G B♭) E4 too, chromatically E♭4.
    let input = sine(0.8, 63.3);
    let settle = |r: &mut Rig<PitchCorrectProcessor>| {
        let l = latency_frames(r) as f64 / SR;
        let out = play(r, &input);
        mean_between(&out, l + 0.2, 0.78).0
    };
    let mut r = rig(&[(id::TARGET, target::KEY_TRACK as f64), (id::SPEED, 0.0)]);
    r.harmony.keys = vec![(0, Key::new(0, Scale::Major))];
    assert!((settle(&mut r) - 64.0).abs() < 0.05);
    assert_eq!(r.tap.value(value::ALLOWED) as u16, 0b1010_1011_0101);
    // A minor key (C minor has E♭).
    let mut r = rig(&[(id::TARGET, target::KEY_TRACK as f64), (id::SPEED, 0.0)]);
    r.harmony.keys = vec![(0, Key::new(0, Scale::Minor))];
    assert!((settle(&mut r) - 63.0).abs() < 0.05);
    // The chord track: a D minor chord there (D F A) pulls to D4 or F4.
    let mut r = rig(&[(id::TARGET, target::CHORD_TRACK as f64), (id::SPEED, 0.0)]);
    r.harmony.chords = vec![(0, i64::MAX, Chord::new(2, Quality::Minor))];
    let got = settle(&mut r);
    assert!(
        (got - 62.0).abs() < 0.05 || (got - 65.0).abs() < 0.05,
        "{got}"
    );
    // Custom: only E♭ ticked.
    let mut set: Vec<(u32, f64)> = (0..12).map(|pc| (id::note(pc), 0.0)).collect();
    set.push((id::note(3), 1.0));
    set.extend([(id::TARGET, target::CUSTOM as f64), (id::SPEED, 0.0)]);
    let mut r = rig(&set);
    assert!((settle(&mut r) - 63.0).abs() < 0.05);
}

#[test]
fn midi_notes_are_the_targets_and_none_leaves_the_voice_alone() {
    let input = sine(0.6, 66.6);
    let mut r = rig(&[(id::TARGET, target::MIDI as f64), (id::SPEED, 0.0)]);
    let l = latency_frames(&r) as f64 / SR;
    let out = play(&mut r, &input);
    let (free, _) = mean_between(&out, l + 0.1, 0.58);
    assert!((free - 66.6).abs() < 0.05, "uncorrected: {free}");
    // A held G4 (67) and C4 (60): the nearest is G4.
    let mut notes = MidiBuffer::with_capacity(8);
    for key in [67u8, 60] {
        notes
            .push(TimedMidiEvent::new(
                0,
                MidiEvent::NoteOn {
                    channel: 0,
                    key,
                    velocity: 100,
                },
            ))
            .unwrap();
    }
    r.events = vec![notes];
    let out = play(&mut r, &input);
    let (pulled, _) = mean_between(&out, l + 0.15, 0.58);
    assert!((pulled - 67.0).abs() < 0.05, "{pulled}");
}

#[test]
fn the_tuning_tunes_the_targets() {
    // A tuning that plays every A a quarter tone sharp.
    let mut pitches = *TuningTable::STANDARD.pitches();
    for k in (9..128).step_by(12) {
        pitches[k] += 0.5;
    }
    let mut r = rig(&[(id::TARGET, target::CHROMATIC as f64), (id::SPEED, 0.0)]);
    r.p.set_tuning(&TuningTable::from_pitches(pitches));
    let l = latency_frames(&r) as f64 / SR;
    let out = play(&mut r, &sine(0.8, 69.35));
    let (got, _) = mean_between(&out, l + 0.15, 0.78);
    assert!((got - 69.5).abs() < 0.04, "{got}");
}

#[test]
fn transpose_and_formants_move_as_set() {
    let x = voice(1.2, |_| 57.0);
    let run = |preserve: f64| {
        let mut r = rig(&[
            (id::TARGET, target::CHROMATIC as f64),
            (id::SPEED, 0.0),
            (id::TRANSPOSE, 5.0),
            (id::PRESERVE_FORMANTS, preserve),
        ]);
        let l = latency_frames(&r) as f64 / SR;
        let out = play(&mut r, &x);
        (mean_between(&out, l + 0.2, 1.15).0, rms_hz(&out))
    };
    let base = rms_hz(&x);
    let (kept_pitch, kept) = run(1.0);
    let (moved_pitch, moved) = run(0.0);
    assert!((kept_pitch - 62.0).abs() < 0.06, "{kept_pitch}");
    assert!((moved_pitch - 62.0).abs() < 0.06, "{moved_pitch}");
    assert!((kept / base - 1.0).abs() < 0.08, "kept {kept} vs {base}");
    assert!(moved / base > 1.2, "moved {moved} vs {base}");
}

#[test]
fn it_is_late_by_its_latency_and_passes_what_it_leaves_alone() {
    // Dry: exactly the input, its latency later.
    let mut r = rig(&[(id::MIX, 0.0)]);
    let lat = latency_frames(&r);
    let x = sine(0.4, 62.0);
    let out = play(&mut r, &x);
    for i in lat..x.len() {
        assert!((out[i] - x[i - lat]).abs() < 1e-6, "{i}");
    }
    // Noise is no voice: it comes through at its level.
    let mut r = rig(&[(id::TARGET, target::CHROMATIC as f64), (id::SPEED, 0.0)]);
    let noise: Vec<f32> = (0..(0.6 * SR) as usize)
        .map(|i| (0.2 * white(i, 9)) as f32)
        .collect();
    let out = play(&mut r, &noise);
    let rms =
        |x: &[f32]| (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    let (a, b) = (rms(&noise[4096..]), rms(&out[4096 + lat..]));
    assert!((20.0 * (b / a).log10()).abs() < 1.0, "{a} {b}");
    // A tone in tune is played as it came, late by the latency.
    let mut r = rig(&[(id::TARGET, target::CHROMATIC as f64), (id::SPEED, 0.0)]);
    let x = sine(0.6, 69.0);
    let out = play(&mut r, &x);
    let err: f64 = (8_000..x.len())
        .map(|i| f64::from(out[i] - x[i - lat]).powi(2))
        .sum::<f64>()
        / (x.len() - 8_000) as f64;
    assert!(err.sqrt() < 0.02, "{}", err.sqrt());
    // The latency follows the range.
    assert_eq!(lat, latency_for(RANGES[1].1, SR));
    r.set(id::RANGE, 0.0);
    assert!(latency(&r.params, SR) < lat as u32);
    assert_eq!(BLOCK, 256);
}
