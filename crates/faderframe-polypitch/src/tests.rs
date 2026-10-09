#![allow(clippy::unwrap_used)]

use super::*;

const RATE: f64 = 48_000.0;
const HOP: usize = 512;

/// A harmonic tone (10 partials, 1/h) of `key` (fractional MIDI) over
/// `a..b` seconds, with vibrato of `vib` cents at 5.5 Hz, into `x`.
fn tone(x: &mut [f32], key: f64, a: f64, b: f64, vib: f64, gain: f64) {
    let mut phase = 0.0f64;
    let (i0, i1) = ((a * RATE) as usize, (b * RATE) as usize);
    for i in i0..i1.min(x.len()) {
        let t = (i - i0) as f64 / RATE;
        let cents = vib * (2.0 * std::f64::consts::PI * 5.5 * t).sin();
        let f = hz(key + cents / 100.0);
        phase = (phase + f / RATE).fract();
        let env = (t / 0.01).min(1.0) * ((b - a - t) / 0.01).clamp(0.0, 1.0);
        let mut v = 0.0;
        for h in 1..=10 {
            v += (2.0 * std::f64::consts::PI * phase * f64::from(h)).sin() / f64::from(h);
        }
        x[i] += (v * env * gain) as f32;
    }
}

/// The magnitude (dB) of frequency `f` around second `t` (Hann, 8192).
fn level(x: &[f32], t: f64, f: f64) -> f64 {
    let n = 8192usize;
    let c = (t * RATE) as usize;
    let (mut re, mut im) = (0.0, 0.0);
    for i in 0..n {
        let s = f64::from(x[c - n / 2 + i]);
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
        let a = 2.0 * std::f64::consts::PI * f * i as f64 / RATE;
        re += s * w * a.cos();
        im -= s * w * a.sin();
    }
    20.0 * ((re * re + im * im).sqrt() / n as f64).max(1e-12).log10()
}

/// A note as the renderer takes it: steady at `key`, moved by `semitones`,
/// at `gain`.
fn steady(key: f64, a: f64, b: f64, semitones: f64, gain: f32) -> Moved {
    let (start, end) = ((a * RATE) as i64, (b * RATE) as i64);
    let frames = ((end - start) as usize) / HOP + 1;
    Moved {
        start,
        end,
        hop: HOP,
        f0: vec![hz(key) as f32; frames],
        ratio: vec![2f64.powf(semitones / 12.0) as f32; frames],
        gain: vec![gain; frames],
    }
}

fn chord() -> Vec<f32> {
    let mut x = vec![0.0f32; (2.0 * RATE) as usize];
    for key in [60.0, 64.0, 67.0] {
        tone(&mut x, key, 0.2, 1.6, 0.0, 0.15);
    }
    x
}

#[test]
fn nothing_moved_is_the_input_sample_for_sample() {
    let x = chord();
    let notes = [
        steady(60.0, 0.2, 1.6, 0.0, 1.0),
        steady(64.0, 0.2, 1.6, 0.0, 1.0),
    ];
    let out = render(std::slice::from_ref(&x), RATE, &notes, &mut |_| {});
    assert_eq!(out[0], x);
    // A moved note changes only its span (and the window's reach).
    let notes = [steady(64.0, 0.8, 1.2, -1.0, 1.0)];
    let out = render(std::slice::from_ref(&x), RATE, &notes, &mut |_| {});
    let reach = frame_size(RATE) / 2;
    let (a, b) = ((0.8 * RATE) as usize - reach, (1.2 * RATE) as usize + reach);
    assert_eq!(out[0][..a], x[..a]);
    assert_eq!(out[0][b..], x[b..]);
    assert_ne!(out[0][a..b], x[a..b]);
}

#[test]
fn one_note_of_a_chord_moves_and_the_others_stay() {
    let x = chord();
    let notes = [
        steady(60.0, 0.2, 1.6, 0.0, 1.0),
        steady(64.0, 0.2, 1.6, -1.0, 1.0),
        steady(67.0, 0.2, 1.6, 0.0, 1.0),
    ];
    let out = render(std::slice::from_ref(&x), RATE, &notes, &mut |_| {})
        .pop()
        .unwrap();
    let t = 0.9;
    for h in 1..=3 {
        let h = f64::from(h);
        let (e, eb) = (hz(64.0) * h, hz(63.0) * h);
        let gone = level(&x, t, e) - level(&out, t, e);
        let came = level(&out, t, eb) - level(&x, t, eb);
        assert!(gone > 15.0, "E4 partial {h}: down {gone:.1} dB");
        assert!(came > 15.0, "E♭4 partial {h}: up {came:.1} dB");
        // It arrives about as loud as it left.
        let kept = level(&out, t, eb) - level(&x, t, e);
        assert!(kept.abs() < 3.0, "partial {h} moved at {kept:.1} dB");
    }
    for key in [60.0, 67.0] {
        let f = hz(key);
        let d = level(&out, t, f) - level(&x, t, f);
        assert!(d.abs() < 1.5, "{key}: {d:.2} dB");
    }
}

#[test]
fn a_note_is_taken_out_of_a_chord() {
    let x = chord();
    let notes = [
        steady(60.0, 0.2, 1.6, 0.0, 1.0),
        steady(64.0, 0.2, 1.6, 0.0, 1.0),
        steady(67.0, 0.2, 1.6, 0.0, 0.0),
    ];
    let out = render(std::slice::from_ref(&x), RATE, &notes, &mut |_| {})
        .pop()
        .unwrap();
    let gone = level(&x, 0.9, hz(67.0)) - level(&out, 0.9, hz(67.0));
    assert!(gone > 20.0, "G4 down {gone:.1} dB");
    for key in [60.0, 64.0] {
        let d = level(&out, 0.9, hz(key)) - level(&x, 0.9, hz(key));
        assert!(d.abs() < 1.5, "{key}: {d:.2} dB");
    }
}

#[test]
fn each_note_of_a_chord_is_followed_to_the_cent() {
    let mut x = vec![0.0f32; (2.0 * RATE) as usize];
    // C a fifth of a semitone sharp, E and G in tune.
    for key in [60.2, 64.0, 67.0] {
        tone(&mut x, key, 0.2, 1.6, 0.0, 0.15);
    }
    let heard = [60.0f32, 64.0, 67.0].map(|key| Heard {
        start: (0.3 * RATE) as i64,
        end: (1.5 * RATE) as i64,
        key,
    });
    let tracks = pitch_tracks(&x, RATE, &heard, HOP);
    for (track, truth) in tracks.iter().zip([60.2, 64.0, 67.0]) {
        let known: Vec<f32> = track.iter().copied().filter(|v| v.is_finite()).collect();
        assert!(known.len() * 10 >= track.len() * 9, "heard throughout");
        for v in known {
            assert!((f64::from(v) - truth).abs() < 0.03, "{v} for {truth}");
        }
    }
}

#[test]
fn a_wavering_note_comes_out_straight() {
    let mut x = vec![0.0f32; (2.0 * RATE) as usize];
    tone(&mut x, 57.0, 0.2, 1.6, 50.0, 0.3);
    let span = Heard {
        start: (0.3 * RATE) as i64,
        end: (1.5 * RATE) as i64,
        key: 57.0,
    };
    let sung = pitch_tracks(&x, RATE, &[span], HOP).pop().unwrap();
    let spread = |t: &[f32]| {
        let v: Vec<f64> = t
            .iter()
            .filter(|v| v.is_finite())
            .map(|v| f64::from(*v))
            .collect();
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt() * 100.0
    };
    let before = spread(&sung);
    assert!(before > 25.0, "it wavers: {before:.1} cents");
    // Each frame moved to 57 exactly.
    let note = Moved {
        start: span.start,
        end: span.end,
        hop: HOP,
        f0: sung
            .iter()
            .map(|k| {
                if k.is_finite() {
                    hz(f64::from(*k)) as f32
                } else {
                    0.0
                }
            })
            .collect(),
        ratio: sung
            .iter()
            .map(|k| {
                if k.is_finite() {
                    (hz(57.0) / hz(f64::from(*k))) as f32
                } else {
                    1.0
                }
            })
            .collect(),
        gain: vec![1.0; sung.len()],
    };
    let out = render(&[x], RATE, &[note], &mut |_| {}).pop().unwrap();
    let after = spread(&pitch_tracks(&out, RATE, &[span], HOP).pop().unwrap());
    assert!(
        after < 6.0,
        "straightened to {after:.1} cents (from {before:.1})"
    );
}
